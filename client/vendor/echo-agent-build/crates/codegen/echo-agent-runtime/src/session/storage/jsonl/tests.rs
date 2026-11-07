#![cfg_attr(rustfmt, rustfmt::skip)]
use super::*;
use crate::session::info::Info;
use crate::session::persistence::default_model_id;
use crate::session::storage::{CopySessionOptions, SessionUpdate};
use crate::tools::todo::TodoState;
use agent_client_protocol as acp;
use tempfile::TempDir;
fn create_test_info() -> Info {
    Info {
        id: acp::SessionId::new("test-session-123"),
        cwd: "/test/workspace".to_string(),
    }
}
fn create_test_chat_messages() -> Vec<ConversationItem> {
    vec![
            ConversationItem::user("Hello world"),
            ConversationItem::user("How are you?"),
            ConversationItem::user("Test message"),
        ]
}
fn create_test_notification() -> acp::SessionNotification {
    acp::SessionNotification::new(
        acp::SessionId::new("test-session-123"),
        acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(
                    acp::TextContent::new("Test response".to_string()),
                ),
            ),
        ),
    )
}
fn create_test_plan_state() -> TodoState {
    TodoState::default()
}
#[tokio::test]
async fn write_compaction_segment_numbers_and_indexes_resume_safely() {
    use crate::extensions::notification::CompactionSegmentFile;
    use echo_agent_sampling_types::ConversationItem;
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let seg = |summary: &str| CompactionSegmentFile {
        items: vec![ConversationItem::user("a"), ConversationItem::user("b")],
        summary: summary.to_string(),
        detail: echo_agent_chat_state::CompactionDetail::Verbose,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
    };
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.write_compaction_segment(&info, &seg("first")).await.unwrap();
    adapter.write_compaction_segment(&info, &seg("second")).await.unwrap();
    let base = adapter
        .session_dir(&info)
        .join(echo_agent_compaction_transcript::COMPACTION_DIR);
    let read = |p: &str| std::fs::read_to_string(base.join(p)).unwrap();
    assert!(read("segment_000.md").contains("# HISTORICAL -- DO NOT EDIT"));
    assert!(read("segment_001.md").contains("second"));
    let index = read("INDEX.md");
    assert_eq!(
            index.matches("# Compaction Segment Index").count(),
            1,
            "title + header written exactly once"
        );
    assert!(index.contains("| 000 | segment_000.md | 2 |"));
    assert!(index.contains("| 001 | segment_001.md | 2 |"));
    let resumed = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    resumed.write_compaction_segment(&info, &seg("third")).await.unwrap();
    assert!(read("segment_000.md").contains("first"));
    assert!(base.join("segment_002.md").exists());
    let index = read("INDEX.md");
    assert_eq!(index.matches("# Compaction Segment Index").count(), 1);
    assert_eq!(index.lines().filter(|l| l.contains("segment_")).count(), 3);
}
#[tokio::test]
async fn update_current_model_persists_leaves_and_clears_reasoning_effort() {
    use echo_agent_sampling_types::ReasoningEffort;
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    let model = default_model_id();
    adapter.init_session(&info, model.clone()).await.unwrap();
    adapter
        .update_current_model_and_agent(
            &info,
            &model,
            None,
            Some(Some(ReasoningEffort::High)),
        )
        .await
        .unwrap();
    assert_eq!(
            adapter.read_summary_sync(&info).unwrap().reasoning_effort,
            Some(ReasoningEffort::High),
        );
    adapter.update_current_model(&info, &model).await.unwrap();
    assert_eq!(
            adapter.read_summary_sync(&info).unwrap().reasoning_effort,
            Some(ReasoningEffort::High),
            "model-only update must not wipe the persisted effort",
        );
    adapter
        .update_current_model_and_agent(&info, &model, None, Some(None))
        .await
        .unwrap();
    assert_eq!(
            adapter.read_summary_sync(&info).unwrap().reasoning_effort,
            None,
        );
}
#[tokio::test]
async fn test_jsonl_round_trip() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    let summary = adapter.init_session(&info, default_model_id()).await.unwrap();
    assert_eq!(summary.info.id, info.id);
    assert_eq!(summary.current_model_id, default_model_id());
    let messages = create_test_chat_messages();
    for msg in &messages {
        adapter.append_chat_message(&info, msg).await.unwrap();
    }
    let notification = create_test_notification();
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(notification)))
        .await
        .unwrap();
    let plan_state = create_test_plan_state();
    adapter.write_plan_state(&info, &plan_state).await.unwrap();
    let new_model = acp::ModelId::new("echo-agent-4.3");
    adapter.update_current_model(&info, &new_model).await.unwrap();
    let loaded = adapter.load_session(&info).await.unwrap();
    assert_eq!(loaded.summary.info.id, info.id);
    assert_eq!(loaded.summary.current_model_id, new_model);
    assert_eq!(loaded.chat_history.len(), messages.len());
    assert_eq!(loaded.updates.len(), 1);
    assert!(loaded.plan_state.is_some());
}
/// Resume from updates.jsonl alone: when chat_history.jsonl is missing, load
/// rebuilds it from the ACP update stream (the durable source of truth).
#[tokio::test]
async fn load_rebuilds_chat_history_from_updates() {
    use agent_client_protocol::{
        ContentBlock, ContentChunk, SessionUpdate as Acp, TextContent,
    };
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let text = |s: &str| ContentChunk::new(
        ContentBlock::Text(TextContent::new(s.to_string())),
    );
    let notify = |u| SessionUpdate::Acp(
        Box::new(acp::SessionNotification::new(info.id.clone(), u)),
    );
    adapter
        .append_update(&info, &notify(Acp::UserMessageChunk(text("ping"))))
        .await
        .unwrap();
    adapter
        .append_update(&info, &notify(Acp::AgentMessageChunk(text("pong"))))
        .await
        .unwrap();
    let chat_path = adapter.session_dir(&info).join("chat_history.jsonl");
    assert_eq!(std::fs::metadata(&chat_path).map(|m| m.len()).unwrap_or(0), 0);
    let loaded = adapter.load_session(&info).await.unwrap();
    assert_eq!(loaded.chat_history.len(), 2, "one user + one agent conversation item");
    assert!(matches!(loaded.chat_history[0], ConversationItem::User(_)));
    assert!(matches!(loaded.chat_history[1], ConversationItem::Assistant(_)));
    let persisted = std::fs::read_to_string(&chat_path).unwrap();
    assert!(
            persisted.contains("ping") && persisted.contains("pong"),
            "rebuilt cache carries the transcript text"
        );
}
#[tokio::test]
async fn workflow_run_manifest_round_trips_and_clear_tombstone_wins() {
    use crate::session::workflow::store::{
        script_revision_path, WorkflowRunManifest, WORKFLOW_RUN_MANIFEST_VERSION,
    };
    use crate::session::workflow::tracker::WorkflowTracker;
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let mut tracker = WorkflowTracker::default();
    let state = tracker
        .start_run(
            "wf_restore".into(),
            "demo".into(),
            "ship".into(),
            Vec::new(),
            None,
            Some("workflows/wf_restore/journal.jsonl".into()),
        );
    let run_dir = adapter.session_dir(&info).join("workflows/wf_restore");
    std::fs::create_dir_all(run_dir.join("scripts")).unwrap();
    std::fs::write(script_revision_path(&run_dir, 0), "complete(\"ok\");").unwrap();
    std::fs::write(run_dir.join("args.json"), r#"{"objective":"ship"}"#).unwrap();
    let manifest = WorkflowRunManifest {
        version: WORKFLOW_RUN_MANIFEST_VERSION,
        state,
        script_revision: 0,
    };
    adapter.write_workflow_run_state(&info, &manifest).await.unwrap();
    let loaded = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(loaded.workflow_runs.len(), 1);
    assert_eq!(loaded.workflow_runs[0].script, "complete(\"ok\");");
    assert_eq!(loaded.workflow_runs[0].args, serde_json::json!({"objective": "ship"}));
    assert_eq!(loaded.workflow_runs[0].effort, None);
    let effort_path = run_dir.join("effort");
    std::fs::write(&effort_path, "high").unwrap();
    let loaded_with_effort = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(loaded_with_effort.workflow_runs[0].effort,
            Some(echo_agent_sampling_types::ReasoningEffort::High));
    for invalid in ["XHIGH", "turbo"] {
        std::fs::write(&effort_path, invalid).unwrap();
        assert!(
                adapter
                    .load_session_without_updates(&info)
                    .await
                    .unwrap()
                    .workflow_runs
                    .is_empty(),
                "present effort sidecar must be canonical: {invalid}"
            );
    }
    std::fs::remove_file(&effort_path).unwrap();
    std::fs::create_dir(&effort_path).unwrap();
    assert!(adapter.load_session_without_updates(&info).await.unwrap().workflow_runs.is_empty());
    std::fs::remove_dir(&effort_path).unwrap();
    std::fs::write(&effort_path, [0xff]).unwrap();
    assert!(adapter.load_session_without_updates(&info).await.unwrap().workflow_runs.is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let effort_target = run_dir.join("effort-target");
        std::fs::write(&effort_target, "high").unwrap();
        std::fs::remove_file(&effort_path).unwrap();
        symlink(&effort_target, &effort_path).unwrap();
        assert!(adapter.load_session_without_updates(&info).await.unwrap().workflow_runs.is_empty());
        std::fs::remove_file(&effort_path).unwrap();
        std::fs::remove_file(&effort_target).unwrap();
    }
    std::fs::write(&effort_path, "high").unwrap();
    let mut legacy = manifest.clone();
    legacy.version = 2;
    adapter.write_workflow_run_state(&info, &legacy).await.unwrap();
    let loaded_v2 = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(loaded_v2.workflow_runs.len(), 1);
    assert_eq!(loaded_v2.workflow_runs[0].manifest.version, 2);
    adapter.delete_workflow_run_state(&info, "wf_restore").await.unwrap();
    adapter.write_workflow_run_state(&info, &manifest).await.unwrap();
    assert!(run_dir.join("cleared").is_file());
    assert!(
            adapter
                .load_session_without_updates(&info)
                .await
                .unwrap()
                .workflow_runs
                .is_empty()
        );
}
#[cfg(unix)]
#[tokio::test]
async fn workflow_restore_rejects_symlinks_and_caps_run_count() {
    use std::os::unix::fs::symlink;
    use crate::session::workflow::store::{
        MAX_RESTORED_WORKFLOW_RUNS, WORKFLOW_RUN_MANIFEST_VERSION, WorkflowRunManifest,
        script_revision_path,
    };
    use crate::session::workflow::tracker::WorkflowTracker;
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let workflows = adapter.session_dir(&info).join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    for index in 0..=MAX_RESTORED_WORKFLOW_RUNS {
        let run_id = format!("wf_{index:03}");
        let run_dir = workflows.join(&run_id);
        std::fs::create_dir_all(run_dir.join("scripts")).unwrap();
        let mut tracker = WorkflowTracker::default();
        let state = tracker
            .start_run(
                run_id.clone(),
                "demo".into(),
                "ship".into(),
                Vec::new(),
                None,
                Some(format!("workflows/{run_id}/journal.jsonl")),
            );
        let manifest = WorkflowRunManifest {
            version: WORKFLOW_RUN_MANIFEST_VERSION,
            state,
            script_revision: 0,
        };
        std::fs::write(
                run_dir.join("state.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
        std::fs::write(script_revision_path(&run_dir, 0), "complete(\"ok\");").unwrap();
        std::fs::write(run_dir.join("args.json"), "{}").unwrap();
    }
    let attacker = temp_dir.path().join("attacker.json");
    std::fs::write(&attacker, "{}").unwrap();
    let symlinked = workflows.join("wf_symlink");
    std::fs::create_dir_all(symlinked.join("scripts")).unwrap();
    symlink(&attacker, symlinked.join("state.json")).unwrap();
    let loaded = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(loaded.workflow_runs.len(), MAX_RESTORED_WORKFLOW_RUNS);
    assert!(
            loaded
                .workflow_runs
                .iter()
                .all(|run| run.manifest.state.run_id != "wf_symlink")
        );
}
/// `load_session_without_updates` always defers rewind points while the full
/// `load_session` / `load_rewind_points` still return them.
#[tokio::test]
async fn load_session_without_updates_defers_rewind_points() {
    use echo_agent_workspace::session::file_state::RewindPoint;
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    adapter.append_rewind_point(&info, &RewindPoint::new(0)).await.unwrap();
    adapter.append_rewind_point(&info, &RewindPoint::new(1)).await.unwrap();
    adapter.load_session_without_updates(&info).await.unwrap();
    let full = adapter.load_session(&info).await.unwrap();
    assert_eq!(full.rewind_points.len(), 2);
    assert_eq!(adapter.load_rewind_points(&info).await.unwrap().len(), 2);
    let path = adapter.rewind_points_file_path(&info).unwrap();
    assert!(path.ends_with("rewind_points.jsonl"));
}
/// The disk-authoritative ConversationOnly merge persists the correct
/// merged/truncated set.
#[tokio::test]
async fn merge_rewind_points_from_persists_merged_set() {
    use echo_agent_workspace::session::file_state::RewindPoint;
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    for i in 0..3 {
        adapter.append_rewind_point(&info, &RewindPoint::new(i)).await.unwrap();
    }
    adapter.merge_rewind_points_from(&info, 1).await.unwrap();
    let after = adapter.load_rewind_points(&info).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].prompt_index, 0);
}
/// A malformed on-disk line makes the STRICT merge read abort BEFORE writing,
/// leaving `rewind_points.jsonl` untouched (never drop the line).
#[tokio::test]
async fn merge_rewind_points_from_aborts_on_malformed_without_writing() {
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let path = adapter.rewind_points_file_path(&info).unwrap();
    let original = "garbage{not json\n";
    tokio::fs::write(&path, original).await.unwrap();
    let res = adapter.merge_rewind_points_from(&info, 1).await;
    assert!(res.is_err(), "malformed read must abort the merge");
    assert_eq!(
            tokio::fs::read_to_string(&path).await.unwrap(),
            original,
            "rewind_points.jsonl must be preserved when the merge aborts"
        );
}
/// File-content `file_snapshots` must round-trip through the on-disk
/// read-modify-write merge (not just index/count).
#[tokio::test]
async fn merge_rewind_points_from_round_trips_file_snapshots() {
    use echo_agent_paths::RelPathBuf;
    use echo_agent_workspace::session::file_state::{FileSnapshot, RewindPoint};
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let mut p0 = RewindPoint::new(0);
    p0.add_snapshot(
        FileSnapshot::new(RelPathBuf::new("a.rs").unwrap(), Some("a-v0".into())),
    );
    let mut p1 = RewindPoint::new(1);
    p1.add_snapshot(
        FileSnapshot::new(RelPathBuf::new("b.rs").unwrap(), Some("b-v1".into())),
    );
    adapter.append_rewind_point(&info, &p0).await.unwrap();
    adapter.append_rewind_point(&info, &p1).await.unwrap();
    adapter.merge_rewind_points_from(&info, 1).await.unwrap();
    let after = adapter.load_rewind_points(&info).await.unwrap();
    assert_eq!(after.len(), 1);
    let m0 = &after[0];
    assert_eq!(m0.prompt_index, 0);
    assert_eq!(
            m0.get_snapshot_by_rel(&RelPathBuf::new("a.rs").unwrap())
                .unwrap()
                .content,
            Some("a-v0".into())
        );
    assert_eq!(
            m0.get_snapshot_by_rel(&RelPathBuf::new("b.rs").unwrap())
                .unwrap()
                .content,
            Some("b-v1".into())
        );
}
/// A `write_jsonl`-backed rewrite (here `truncate_rewind_points_from`) renames
/// the target into place and leaves NO `*.jsonl.tmp` behind.
#[tokio::test]
async fn write_jsonl_leaves_no_temp_and_renames_target() {
    use echo_agent_workspace::session::file_state::RewindPoint;
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    for i in 0..3 {
        adapter.append_rewind_point(&info, &RewindPoint::new(i)).await.unwrap();
    }
    adapter.truncate_rewind_points_from(&info, 2).await.unwrap();
    let kept = adapter.load_rewind_points(&info).await.unwrap();
    assert_eq!(
            kept.iter().map(|p| p.prompt_index).collect::<Vec<_>>(),
            vec![0, 1]
        );
    let path = adapter.rewind_points_file_path(&info).unwrap();
    let leftover_tmps: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(
            leftover_tmps.is_empty(),
            "no *.tmp should remain after write_jsonl: {leftover_tmps:?}"
        );
}
/// The resume/read paths must not mutate the on-disk `updates.jsonl` or
/// `rewind_points.jsonl`, and ACU lines stay on disk.
#[tokio::test]
async fn reads_never_modify_rewind_or_updates_files() {
    use echo_agent_workspace::session::file_state::{FileStateTracker, RewindPoint};
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    adapter.append_rewind_point(&info, &RewindPoint::new(0)).await.unwrap();
    adapter.append_rewind_point(&info, &RewindPoint::new(1)).await.unwrap();
    let updates_path = adapter.updates_file_path(&info).unwrap();
    let acu = r#"{"timestamp":0,"method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"available_commands_update","availableCommands":[]}}}"#;
    tokio::fs::write(&updates_path, format!("{acu}\n")).await.unwrap();
    let rewind_path = adapter.rewind_points_file_path(&info).unwrap();
    let rewind_before = std::fs::read(&rewind_path).unwrap();
    let updates_before = std::fs::read(&updates_path).unwrap();
    adapter.load_session_without_updates(&info).await.unwrap();
    let tracker = FileStateTracker::with_lazy_source(rewind_path.clone());
    assert_eq!(tracker.get_rewind_points().await.len(), 2);
    assert_eq!(
            std::fs::read(&rewind_path).unwrap(),
            rewind_before,
            "rewind_points.jsonl must be unchanged by reads"
        );
    assert_eq!(
            std::fs::read(&updates_path).unwrap(),
            updates_before,
            "updates.jsonl must be unchanged by reads"
        );
    let updates_str = String::from_utf8(updates_before).unwrap();
    assert!(
            updates_str.contains("available_commands_update"),
            "ACU stays persisted on disk (only skipped on forward)"
        );
}
#[tokio::test]
async fn delete_session_removes_dir_and_is_idempotent() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let dir = adapter.session_dir(&info);
    assert!(dir.exists(), "session dir should exist after init");
    adapter.delete_session(&info).await.unwrap();
    assert!(!dir.exists(), "session dir should be gone after delete");
    assert!(
            adapter.load_summary(&info).await.is_err(),
            "summary must not load after delete"
        );
    adapter.delete_session(&info).await.expect("second delete must succeed");
}
#[tokio::test]
async fn test_echo_agent_session_update_round_trip() {
    use crate::extensions::notification::{
        DiffContent, SessionNotification as EchoAgentSessionNotification,
        SessionUpdate as EchoAgentSessionUpdateType,
    };
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let echo_agent_notification = EchoAgentSessionNotification {
        session_id: acp::SessionId::new("test-session-123"),
        update: EchoAgentSessionUpdateType::DiffReview {
            content: vec![DiffContent {
                    diff: acp::Diff::new(
                        std::path::PathBuf::from("/test/file.rs"),
                        "new code".to_string(),
                    )
                    .old_text(Some("old code".to_string())),
                }],
        },
        meta: None,
    };
    adapter
        .append_update(&info, &SessionUpdate::EchoAgent(Box::new(echo_agent_notification.clone())))
        .await
        .unwrap();
    let acp_notification = create_test_notification();
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(acp_notification)))
        .await
        .unwrap();
    let loaded = adapter.load_session(&info).await.unwrap();
    assert_eq!(
            loaded.updates.len(),
            2,
            "Should have 2 updates (1 EchoAgent + 1 ACP)"
        );
    match &loaded.updates[0] {
        SessionUpdate::EchoAgent(notification) => {
            assert_eq!(notification.session_id.0.as_ref(), "test-session-123");
            match &notification.update {
                EchoAgentSessionUpdateType::DiffReview { content } => {
                    assert_eq!(content.len(), 1);
                    assert_eq!(
                            content[0].diff.path,
                            std::path::PathBuf::from("/test/file.rs")
                        );
                }
                _ => {
                    panic!("Expected DiffReview, got different update type");
                }
            }
        }
        _ => panic!("Expected EchoAgent update as first item"),
    }
    match &loaded.updates[1] {
        SessionUpdate::Acp(_) => {}
        _ => panic!("Expected ACP update as second item"),
    }
}
/// SubagentSpawned and SubagentFinished must survive JSONL round-trip
/// with exact field preservation.
#[tokio::test]
async fn test_subagent_notifications_round_trip() {
    use crate::extensions::notification::{
        SessionNotification as EchoAgentSessionNotification,
        SessionUpdate as EchoAgentSessionUpdateType,
    };
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let spawned = EchoAgentSessionNotification {
        session_id: acp::SessionId::new("parent-session"),
        update: EchoAgentSessionUpdateType::SubagentSpawned {
            subagent_id: "child-001".to_string(),
            parent_session_id: "parent-session".to_string(),
            parent_prompt_id: Some("turn-123".to_string()),
            child_session_id: "child-001".to_string(),
            subagent_type: "general-purpose".to_string(),
            description: "Read README.md".to_string(),
            effective_context_source: None,
            context_normalized: false,
            capability_mode: None,
            persona: None,
            role: None,
            model: None,
            resumed_from: None,
            workflow_run_id: None,
        },
        meta: None,
    };
    adapter.append_update(&info, &SessionUpdate::EchoAgent(Box::new(spawned))).await.unwrap();
    let finished = EchoAgentSessionNotification {
        session_id: acp::SessionId::new("parent-session"),
        update: EchoAgentSessionUpdateType::SubagentFinished {
            subagent_id: "child-001".to_string(),
            child_session_id: "child-001".to_string(),
            status: "completed".to_string(),
            error: None,
            tool_calls: 5,
            turns: 2,
            duration_ms: 12345,
            tokens_used: 0,
            output: None,
            will_wake: false,
        },
        meta: None,
    };
    adapter.append_update(&info, &SessionUpdate::EchoAgent(Box::new(finished))).await.unwrap();
    let loaded = adapter.load_session(&info).await.unwrap();
    assert_eq!(loaded.updates.len(), 2);
    match &loaded.updates[0] {
        SessionUpdate::EchoAgent(notification) => {
            match &notification.update {
                EchoAgentSessionUpdateType::SubagentSpawned {
                    subagent_id,
                    child_session_id,
                    description,
                    subagent_type,
                    ..
                } => {
                    assert_eq!(subagent_id, "child-001");
                    assert_eq!(child_session_id, "child-001");
                    assert_eq!(description, "Read README.md");
                    assert_eq!(subagent_type, "general-purpose");
                }
                other => panic!("Expected SubagentSpawned, got {other:?}"),
            }
        }
        other => panic!("Expected EchoAgent update, got {other:?}"),
    }
    match &loaded.updates[1] {
        SessionUpdate::EchoAgent(notification) => {
            match &notification.update {
                EchoAgentSessionUpdateType::SubagentFinished {
                    subagent_id,
                    status,
                    tool_calls,
                    turns,
                    duration_ms,
                    error,
                    ..
                } => {
                    assert_eq!(subagent_id, "child-001");
                    assert_eq!(status, "completed");
                    assert_eq!(*tool_calls, 5);
                    assert_eq!(*turns, 2);
                    assert_eq!(*duration_ms, 12345);
                    assert!(error.is_none());
                }
                other => panic!("Expected SubagentFinished, got {other:?}"),
            }
        }
        other => panic!("Expected EchoAgent update, got {other:?}"),
    }
    let raw_jsonl = tokio::fs::read_to_string(
            adapter.session_dir(&info).join("updates.jsonl"),
        )
        .await
        .unwrap();
    let lines: Vec<&str> = raw_jsonl.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
            lines.len(),
            2,
            "Expected 2 JSONL lines (spawned + finished), got {}",
            lines.len()
        );
    let spawned_json: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(spawned_json["method"], "_echo.agent/session/update");
    let spawned_update = &spawned_json["params"]["update"];
    assert_eq!(spawned_update["sessionUpdate"], "subagent_spawned");
    assert_eq!(spawned_update["subagent_id"], "child-001");
    let finished_json: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(finished_json["method"], "_echo.agent/session/update");
    let finished_update = &finished_json["params"]["update"];
    assert_eq!(finished_update["sessionUpdate"], "subagent_finished");
    assert_eq!(finished_update["tool_calls"], 5);
    assert_eq!(finished_update["duration_ms"], 12345);
}
#[tokio::test]
async fn test_subagent_spawned_resumed_roundtrip() {
    use crate::extensions::notification::{
        SessionNotification as EchoAgentSessionNotification,
        SessionUpdate as EchoAgentSessionUpdateType,
    };
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let spawned = EchoAgentSessionNotification {
        session_id: acp::SessionId::new("resume-parent"),
        update: EchoAgentSessionUpdateType::SubagentSpawned {
            subagent_id: "child-resumed".to_string(),
            parent_session_id: "resume-parent".to_string(),
            parent_prompt_id: Some("turn-5".to_string()),
            child_session_id: "child-resumed".to_string(),
            subagent_type: "general-purpose".to_string(),
            description: "fix review feedback".to_string(),
            effective_context_source: Some("resumed".to_string()),
            context_normalized: false,
            capability_mode: None,
            persona: Some("implementer".to_string()),
            role: None,
            model: None,
            resumed_from: Some("source-agent-id".to_string()),
            workflow_run_id: None,
        },
        meta: None,
    };
    adapter.append_update(&info, &SessionUpdate::EchoAgent(Box::new(spawned))).await.unwrap();
    let loaded = adapter.load_session(&info).await.unwrap();
    assert_eq!(loaded.updates.len(), 1);
    match &loaded.updates[0] {
        SessionUpdate::EchoAgent(notification) => {
            match &notification.update {
                EchoAgentSessionUpdateType::SubagentSpawned {
                    subagent_id,
                    effective_context_source,
                    persona,
                    resumed_from,
                    ..
                } => {
                    assert_eq!(subagent_id, "child-resumed");
                    assert_eq!(effective_context_source.as_deref(), Some("resumed"),);
                    assert_eq!(persona.as_deref(), Some("implementer"));
                    assert_eq!(
                        resumed_from.as_deref(),
                        Some("source-agent-id"),
                        "resumed_from should round-trip through JSONL persistence"
                    );
                }
                other => panic!("Expected SubagentSpawned, got {other:?}"),
            }
        }
        other => panic!("Expected EchoAgent update, got {other:?}"),
    }
}
#[tokio::test]
async fn test_load_prompts_only() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let user_prompt1 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(
                    acp::TextContent::new("First user prompt".to_string()),
                ),
            ),
        ),
    );
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(user_prompt1)))
        .await
        .unwrap();
    let agent_msg = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(
                    acp::TextContent::new("Agent response".to_string()),
                ),
            ),
        ),
    );
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(agent_msg)))
        .await
        .unwrap();
    let user_prompt2 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(
                    acp::TextContent::new("Second user prompt".to_string()),
                ),
            ),
        ),
    );
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(user_prompt2)))
        .await
        .unwrap();
    let prompts = adapter.load_prompts_only(&info).await.unwrap();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0], "First user prompt");
    assert_eq!(prompts[1], "Second user prompt");
}
#[tokio::test]
async fn test_load_prompts_only_empty_session() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let prompts = adapter.load_prompts_only(&info).await.unwrap();
    assert!(prompts.is_empty());
}
#[tokio::test]
async fn test_load_prompts_only_nonexistent_session() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = Info {
        id: acp::SessionId::new("nonexistent"),
        cwd: "/nonexistent".to_string(),
    };
    let prompts = adapter.load_prompts_only(&info).await.unwrap();
    assert!(prompts.is_empty());
}
/// A user prompt streamed as two consecutive `UserMessageChunk` updates
/// must be merged into a single prompt string, not split into two.
#[tokio::test]
async fn test_load_prompts_only_merges_multi_chunk_prompt() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = Info {
        id: acp::SessionId::new("multi-chunk-test"),
        cwd: "/test".to_string(),
    };
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let chunk1 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("Hello ".to_string())),
            ),
        ),
    );
    let chunk2 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("world".to_string())),
            ),
        ),
    );
    let agent_reply = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("Hi!".to_string())),
            ),
        ),
    );
    adapter.append_update(&info, &SessionUpdate::Acp(Box::new(chunk1))).await.unwrap();
    adapter.append_update(&info, &SessionUpdate::Acp(Box::new(chunk2))).await.unwrap();
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(agent_reply)))
        .await
        .unwrap();
    let prompts = adapter.load_prompts_only(&info).await.unwrap();
    assert_eq!(
            prompts.len(),
            1,
            "expected 1 merged prompt, got: {prompts:?}"
        );
    assert_eq!(prompts[0], "Hello world");
}
/// `RewindMarker` updates must truncate dead-branch prompts so only the
/// current timeline's prompts are returned.
#[tokio::test]
async fn test_load_prompts_only_applies_rewind_truncation() {
    use crate::extensions::notification::{
        SessionNotification as EchoAgentNotification, SessionUpdate as EchoAgentSessionUpdate,
    };
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = Info {
        id: acp::SessionId::new("rewind-test"),
        cwd: "/test".to_string(),
    };
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let user1 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(
                    acp::TextContent::new("first prompt".to_string()),
                ),
            ),
        ),
    );
    let agent1 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("answer1".to_string())),
            ),
        ),
    );
    let user2 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("dead branch".to_string())),
            ),
        ),
    );
    let agent2 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("answer2".to_string())),
            ),
        ),
    );
    let rewind = EchoAgentNotification {
        session_id: info.id.clone(),
        update: EchoAgentSessionUpdate::RewindMarker {
            target_prompt_index: 1,
            created_at: "2024-01-01T00:00:00Z".to_string(),
        },
        meta: None,
    };
    let user3 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(
                    acp::TextContent::new("new second prompt".to_string()),
                ),
            ),
        ),
    );
    let agent3 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("answer3".to_string())),
            ),
        ),
    );
    for update in [
        SessionUpdate::Acp(Box::new(user1)),
        SessionUpdate::Acp(Box::new(agent1)),
        SessionUpdate::Acp(Box::new(user2)),
        SessionUpdate::Acp(Box::new(agent2)),
        SessionUpdate::EchoAgent(Box::new(rewind)),
        SessionUpdate::Acp(Box::new(user3)),
        SessionUpdate::Acp(Box::new(agent3)),
    ] {
        adapter.append_update(&info, &update).await.unwrap();
    }
    let prompts = adapter.load_prompts_only(&info).await.unwrap();
    assert_eq!(
            prompts,
            vec!["first prompt", "new second prompt"],
            "dead-branch prompt should have been removed by rewind"
        );
}
/// Malformed JSON lines between valid chunks must not break the extraction
/// of surrounding prompts.
#[tokio::test]
async fn test_load_prompts_only_robust_to_malformed_lines() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = Info {
        id: acp::SessionId::new("malformed-test"),
        cwd: "/test".to_string(),
    };
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let user1 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(
                    acp::TextContent::new("valid prompt".to_string()),
                ),
            ),
        ),
    );
    let agent_end = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::AgentMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(acp::TextContent::new("ok".to_string())),
            ),
        ),
    );
    adapter.append_update(&info, &SessionUpdate::Acp(Box::new(user1))).await.unwrap();
    adapter
        .append_update(&info, &SessionUpdate::Acp(Box::new(agent_end)))
        .await
        .unwrap();
    let updates_path = adapter.updates_file_path(&info).unwrap();
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&updates_path)
            .unwrap();
        writeln!(f, "not valid json {{{{{{").unwrap();
    }
    let user2 = acp::SessionNotification::new(
        info.id.clone(),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(
                acp::ContentBlock::Text(
                    acp::TextContent::new("second valid prompt".to_string()),
                ),
            ),
        ),
    );
    adapter.append_update(&info, &SessionUpdate::Acp(Box::new(user2))).await.unwrap();
    let prompts = adapter.load_prompts_only(&info).await.unwrap();
    assert_eq!(
            prompts,
            vec!["valid prompt", "second valid prompt"],
            "malformed line should not drop surrounding valid prompts"
        );
}
/// A large synthetic session with interleaved tool calls extracts
/// correctly; the selective parser never allocates full notifications
/// for the non-user updates that dominate a real file.
#[tokio::test]
async fn test_load_prompts_only_large_session() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = Info {
        id: acp::SessionId::new("large-session-test"),
        cwd: "/test".to_string(),
    };
    adapter.init_session(&info, default_model_id()).await.unwrap();
    const TURNS: usize = 200;
    for i in 0..TURNS {
        let c1 = acp::SessionNotification::new(
            info.id.clone(),
            acp::SessionUpdate::UserMessageChunk(
                acp::ContentChunk::new(
                    acp::ContentBlock::Text(
                        acp::TextContent::new(format!("turn {i} part1 ")),
                    ),
                ),
            ),
        );
        let c2 = acp::SessionNotification::new(
            info.id.clone(),
            acp::SessionUpdate::UserMessageChunk(
                acp::ContentChunk::new(
                    acp::ContentBlock::Text(acp::TextContent::new(format!("part2"))),
                ),
            ),
        );
        let agent = acp::SessionNotification::new(
            info.id.clone(),
            acp::SessionUpdate::AgentMessageChunk(
                acp::ContentChunk::new(
                    acp::ContentBlock::Text(
                        acp::TextContent::new(
                            format!(
                        "agent reply {i} with lots of content xxxxxx"
                    ),
                        ),
                    ),
                ),
            ),
        );
        adapter.append_update(&info, &SessionUpdate::Acp(Box::new(c1))).await.unwrap();
        adapter.append_update(&info, &SessionUpdate::Acp(Box::new(c2))).await.unwrap();
        adapter
            .append_update(&info, &SessionUpdate::Acp(Box::new(agent)))
            .await
            .unwrap();
    }
    let prompts = adapter.load_prompts_only(&info).await.unwrap();
    assert_eq!(
            prompts.len(),
            TURNS,
            "should extract exactly one merged prompt per turn"
        );
    assert_eq!(prompts[0], "turn 0 part1 part2");
    assert_eq!(
            prompts[TURNS - 1],
            format!("turn {} part1 part2", TURNS - 1)
        );
}
#[tokio::test]
async fn test_append_feedback_creates_file_and_persists() {
    use crate::session::persistence::{LocalFeedbackEntry, UserFeedbackEntry};
    use echo_agent_chat_proxy_types::feedback_types::{
        ClientType, FeedbackSubmission, FeedbackType, RatingType,
    };
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let user_entry = LocalFeedbackEntry::UserFeedback(UserFeedbackEntry {
        submitted_at: chrono::Utc::now(),
        session_id: "test-session-123".into(),
        turn_number: Some(3),
        solicited: false,
        request_id: None,
        dismissed: false,
        submission: Some(FeedbackSubmission {
            session_id: "test-session-123".into(),
            client_type: ClientType::Tui,
            feedback_type: FeedbackType::Rating,
            turn_number: Some(3),
            rating_type: Some(RatingType::Thumbs),
            rating_value: Some(1),
            model_id: Some("echo-agent-3-fast".into()),
            resolved_model_id: Some("echo-agent-4.5".into()),
            ..Default::default()
        }),
    });
    adapter.append_feedback(&info, &user_entry).await.unwrap();
    let dismiss_entry = LocalFeedbackEntry::UserFeedback(UserFeedbackEntry {
        submitted_at: chrono::Utc::now(),
        session_id: "test-session-123".into(),
        turn_number: None,
        solicited: true,
        request_id: Some("req-dismiss-1".into()),
        dismissed: true,
        submission: None,
    });
    adapter.append_feedback(&info, &dismiss_entry).await.unwrap();
    let feedback_path = adapter.feedback_file(&info);
    let content = tokio::fs::read_to_string(&feedback_path).await.unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 2, "Expected 2 JSONL lines");
    let parsed0: LocalFeedbackEntry = serde_json::from_str(lines[0]).unwrap();
    assert!(matches!(parsed0, LocalFeedbackEntry::UserFeedback(_)));
    let parsed1: LocalFeedbackEntry = serde_json::from_str(lines[1]).unwrap();
    let LocalFeedbackEntry::UserFeedback(ref uf) = parsed1;
    assert!(uf.dismissed);
    assert!(uf.submission.is_none());
}
#[tokio::test]
async fn summary_provenance_survives_write_read_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let info = Info {
        id: acp::SessionId::new("prov-rt"),
        cwd: "/test".to_string(),
    };
    let mut summary = adapter.init_session(&info, default_model_id()).await.unwrap();
    summary.fork_context_source = Some("forked".to_string());
    summary.fork_parent_prompt_id = Some("prompt-99".to_string());
    summary.session_kind = Some("subagent_fork".to_string());
    let json = serde_json::to_vec_pretty(&summary).unwrap();
    std::fs::write(adapter.session_dir(&info).join("summary.json"), json).unwrap();
    let loaded = adapter.load_session(&info).await.unwrap();
    assert_eq!(
            loaded.summary.fork_context_source.as_deref(),
            Some("forked")
        );
    assert_eq!(
            loaded.summary.fork_parent_prompt_id.as_deref(),
            Some("prompt-99")
        );
    assert_eq!(
            loaded.summary.session_kind.as_deref(),
            Some("subagent_fork")
        );
}
#[tokio::test]
async fn summary_provenance_defaults_to_none() {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let info = Info {
        id: acp::SessionId::new("prov-none"),
        cwd: "/test".to_string(),
    };
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let loaded = adapter.load_session(&info).await.unwrap();
    assert!(loaded.summary.fork_context_source.is_none());
    assert!(loaded.summary.fork_parent_prompt_id.is_none());
}
#[tokio::test]
async fn init_session_stamps_configured_profile_on_new_session() {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    echo_agent_sandbox::set_configured_profile("workspace");
    let expected = echo_agent_sandbox::configured_profile_name().map(String::from);
    let info = Info {
        id: acp::SessionId::new("new-sb"),
        cwd: "/new".to_string(),
    };
    let summary = adapter.init_session(&info, default_model_id()).await.unwrap();
    assert_eq!(summary.sandbox_profile, expected);
    let on_disk = adapter.read_summary_sync(&info).unwrap();
    assert_eq!(on_disk.sandbox_profile, expected);
}
/// Create a minimal on-disk session directory with a summary.json.
/// Returns the path to the session directory.
fn write_test_summary(
    root: &std::path::Path,
    cwd_encoded: &str,
    session_id: &str,
    updated_at: chrono::DateTime<chrono::Utc>,
    last_active_at: Option<chrono::DateTime<chrono::Utc>>,
    hidden: Option<bool>,
    session_kind: Option<&str>,
) -> PathBuf {
    let session_dir = root.join("sessions").join(cwd_encoded).join(session_id);
    std::fs::create_dir_all(&session_dir).unwrap();
    let summary = Summary {
        info: Info {
            id: acp::SessionId::new(session_id),
            cwd: urlencoding::decode(cwd_encoded).unwrap().into_owned(),
        },
        cwd_generation: 0,
        previous_cwd: None,
        pending_cwd_switch_reminder: None,
        cwd_switch_bookkeeping_generation: 0,
        session_summary: format!("summary for {session_id}"),
        created_at: updated_at,
        updated_at,
        num_messages: 1,
        num_chat_messages: 1,
        current_model_id: default_model_id(),
        parent_session_id: None,
        forked_at: None,
        collection_id: None,
        next_trace_turn: 0,
        chat_format_version: 0,
        prompt_display_cwd: None,
        session_kind: session_kind.map(|s| s.to_string()),
        fork_context_source: None,
        fork_parent_prompt_id: None,
        inherited_prefix_len: None,
        title_source_prompt_index: None,
        hidden,
        source_workspace_dir: None,
        git_root_dir: None,
        git_remotes: Vec::new(),
        head_commit: None,
        head_branch: None,
        request_id: None,
        echo_agent_home: None,
        last_active_at,
        generated_title: None,
        title_is_manual: false,
        worktree_label: None,
        agent_name: None,
        sandbox_profile: None,
        reasoning_effort: None,
        last_turn_summary: None,
        last_turn_summary_prompt_id: None,
        last_recap: None,
    };
    let json = serde_json::to_vec_pretty(&summary).unwrap();
    std::fs::write(session_dir.join("summary.json"), json).unwrap();
    session_dir
}
#[test]
fn scan_session_dirs_returns_empty_for_explicit_mode() {
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(PathBuf::from("/fake"));
    assert!(adapter.scan_session_dirs(None).unwrap().is_empty());
}
#[test]
fn scan_session_dirs_returns_empty_when_no_sessions_dir() {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    assert!(adapter.scan_session_dirs(None).unwrap().is_empty());
}
#[test]
fn scan_session_dirs_finds_all_sessions() {
    let tmp = TempDir::new().unwrap();
    let now = chrono::Utc::now();
    let cwd = crate::util::echo_agent_home::encode_cwd_dirname("/home/user/project");
    write_test_summary(tmp.path(), &cwd, "s1", now, None, None, None);
    write_test_summary(tmp.path(), &cwd, "s2", now, None, None, None);
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let dirs = adapter.scan_session_dirs(None).unwrap();
    assert_eq!(dirs.len(), 2);
}
#[test]
fn scan_session_dirs_filters_by_cwd() {
    let tmp = TempDir::new().unwrap();
    let now = chrono::Utc::now();
    let cwd_a = crate::util::echo_agent_home::encode_cwd_dirname("/home/user/project-a");
    let cwd_b = crate::util::echo_agent_home::encode_cwd_dirname("/home/user/project-b");
    write_test_summary(tmp.path(), &cwd_a, "s1", now, None, None, None);
    write_test_summary(tmp.path(), &cwd_b, "s2", now, None, None, None);
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let a_dirs = adapter.scan_session_dirs(Some("/home/user/project-a")).unwrap();
    assert_eq!(a_dirs.len(), 1);
    assert!(a_dirs[0].ends_with("s1"));
    let all_dirs = adapter.scan_session_dirs(None).unwrap();
    assert_eq!(all_dirs.len(), 2);
}
#[test]
fn scan_session_dirs_skips_non_directory_entries() {
    let tmp = TempDir::new().unwrap();
    let cwd = crate::util::echo_agent_home::encode_cwd_dirname("/project");
    let cwd_dir = tmp.path().join("sessions").join(&cwd);
    std::fs::create_dir_all(&cwd_dir).unwrap();
    std::fs::write(cwd_dir.join("stray-file.txt"), b"oops").unwrap();
    std::fs::create_dir(cwd_dir.join("real-session")).unwrap();
    std::fs::write(cwd_dir.join("real-session/summary.json"), b"{}").unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let dirs = adapter.scan_session_dirs(None).unwrap();
    assert_eq!(dirs.len(), 1);
    assert!(dirs[0].ends_with("real-session"));
}
#[tokio::test]
async fn list_sessions_recent_returns_most_recent_by_mtime() {
    let tmp = TempDir::new().unwrap();
    let cwd = crate::util::echo_agent_home::encode_cwd_dirname("/workspace");
    let t1 = chrono::Utc::now() - chrono::Duration::hours(3);
    let t2 = chrono::Utc::now() - chrono::Duration::hours(2);
    let t3 = chrono::Utc::now() - chrono::Duration::hours(1);
    let dir1 = write_test_summary(tmp.path(), &cwd, "old", t1, None, None, None);
    let dir2 = write_test_summary(tmp.path(), &cwd, "mid", t2, None, None, None);
    let dir3 = write_test_summary(tmp.path(), &cwd, "new", t3, None, None, None);
    set_mtime(&dir1.join("summary.json"), t1);
    set_mtime(&dir2.join("summary.json"), t2);
    set_mtime(&dir3.join("summary.json"), t3);
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let recent = adapter.list_sessions_recent(2).await.unwrap();
    assert_eq!(recent.len(), 2, "should return at most `limit` sessions");
    assert_eq!(recent[0].info.id, acp::SessionId::new("new"));
    assert_eq!(recent[1].info.id, acp::SessionId::new("mid"));
}
#[tokio::test]
async fn list_sessions_recent_excludes_hidden_sessions() {
    let tmp = TempDir::new().unwrap();
    let cwd = crate::util::echo_agent_home::encode_cwd_dirname("/workspace");
    let now = chrono::Utc::now();
    write_test_summary(tmp.path(), &cwd, "visible", now, None, None, None);
    write_test_summary(tmp.path(), &cwd, "hidden-explicit", now, None, Some(true), None);
    write_test_summary(
        tmp.path(),
        &cwd,
        "hidden-subagent",
        now,
        None,
        None,
        Some("subagent"),
    );
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let recent = adapter.list_sessions_recent(100).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].info.id, acp::SessionId::new("visible"));
}
#[tokio::test]
async fn list_sessions_recent_empty_dir() {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let recent = adapter.list_sessions_recent(10).await.unwrap();
    assert!(recent.is_empty());
}
#[tokio::test]
async fn list_sessions_sorts_by_last_active_at_over_updated_at() {
    let tmp = TempDir::new().unwrap();
    let cwd_path = "/ws/resume-sort";
    let cwd = crate::util::echo_agent_home::encode_cwd_dirname(cwd_path);
    let now = chrono::Utc::now();
    write_test_summary(
        tmp.path(),
        &cwd,
        "stale_activity",
        now,
        Some(now - chrono::Duration::hours(20)),
        None,
        None,
    );
    write_test_summary(
        tmp.path(),
        &cwd,
        "recent_activity",
        now - chrono::Duration::hours(10),
        Some(now - chrono::Duration::hours(1)),
        None,
        None,
    );
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let listed = adapter.list_sessions(Some(cwd_path)).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].info.id, acp::SessionId::new("recent_activity"));
    assert_eq!(listed[1].info.id, acp::SessionId::new("stale_activity"));
}
#[tokio::test]
async fn list_sessions_recent_sorts_by_updated_at() {
    let tmp = TempDir::new().unwrap();
    let cwd = crate::util::echo_agent_home::encode_cwd_dirname("/ws");
    let now = chrono::Utc::now();
    let t_old = now - chrono::Duration::hours(10);
    let t_new = now;
    write_test_summary(tmp.path(), &cwd, "a-old", t_old, None, None, None);
    write_test_summary(tmp.path(), &cwd, "b-new", t_new, None, None, None);
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let recent = adapter.list_sessions_recent(10).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].info.id, acp::SessionId::new("b-new"));
    assert_eq!(recent[1].info.id, acp::SessionId::new("a-old"));
}
#[tokio::test]
async fn list_sessions_recent_spans_multiple_workspaces() {
    let tmp = TempDir::new().unwrap();
    let cwd_a = crate::util::echo_agent_home::encode_cwd_dirname("/project-a");
    let cwd_b = crate::util::echo_agent_home::encode_cwd_dirname("/project-b");
    let now = chrono::Utc::now();
    write_test_summary(
        tmp.path(),
        &cwd_a,
        "a1",
        now - chrono::Duration::hours(1),
        None,
        None,
        None,
    );
    write_test_summary(tmp.path(), &cwd_b, "b1", now, None, None, None);
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let recent = adapter.list_sessions_recent(10).await.unwrap();
    assert_eq!(recent.len(), 2);
    assert_eq!(recent[0].info.id, acp::SessionId::new("b1"));
    assert_eq!(recent[1].info.id, acp::SessionId::new("a1"));
}
#[tokio::test]
async fn list_sessions_recent_skips_corrupt_summary() {
    let tmp = TempDir::new().unwrap();
    let cwd = crate::util::echo_agent_home::encode_cwd_dirname("/ws");
    let now = chrono::Utc::now();
    write_test_summary(tmp.path(), &cwd, "good", now, None, None, None);
    let bad_dir = tmp.path().join("sessions").join(&cwd).join("bad");
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(bad_dir.join("summary.json"), b"not valid json!!!").unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let recent = adapter.list_sessions_recent(10).await.unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].info.id, acp::SessionId::new("good"));
}
/// Helper: set the mtime of a file to a specific chrono DateTime.
fn set_mtime(path: &std::path::Path, time: chrono::DateTime<chrono::Utc>) {
    use std::time::{Duration, UNIX_EPOCH};
    let secs = time.timestamp() as u64;
    let system_time = UNIX_EPOCH + Duration::from_secs(secs);
    let mtime = filetime::FileTime::from_system_time(system_time);
    filetime::set_file_mtime(path, mtime).unwrap();
}
fn test_png_bytes() -> Vec<u8> {
    use image::{ImageBuffer, Rgba};
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(
        32,
        32,
        Rgba([10, 20, 30, 255]),
    );
    let mut buf = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png).unwrap();
    buf
}
fn test_jpeg_bytes() -> Vec<u8> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{DynamicImage, ImageBuffer, Rgb};
    let img: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::from_fn(
        64,
        64,
        |x, y| Rgb([(x ^ y) as u8, x as u8, y as u8]),
    );
    let mut buf = Vec::new();
    JpegEncoder::new_with_quality(&mut buf, 85)
        .encode_image(&DynamicImage::ImageRgb8(img))
        .unwrap();
    buf
}
fn image_data_uri(mime: &str, bytes: &[u8]) -> String {
    use base64::Engine as _;
    format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
}
#[test]
fn strip_invalid_images_valid_data_uri_passes() {
    let url = image_data_uri("image/png", &test_png_bytes());
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Text {
                text: "look".into(),
            },
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 0);
    assert!(matches!(&items[0], ConversationItem::User(u) if u.content.len() == 2));
    assert!(matches!(&items[0], ConversationItem::User(u)
            if matches!(&u.content[1], ContentPart::Image { .. })));
}
#[test]
fn strip_invalid_images_corrupt_base64_stripped() {
    let url = "data:image/png;base64,!!!not-valid-base64!!!".to_string();
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Text {
                text: "look".into(),
            },
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
    if let ConversationItem::User(u) = &items[0] {
        assert_eq!(u.content.len(), 2);
        assert!(
                matches!(&u.content[1], ContentPart::Text { text } if text.contains("invalid data"))
            );
    } else {
        panic!("expected User");
    }
}
#[test]
fn strip_invalid_images_malformed_data_uri_no_base64_marker() {
    let url = "data:image/png,rawbytes".to_string();
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
    assert!(matches!(
            &items[0],
            ConversationItem::User(u) if matches!(&u.content[0], ContentPart::Text { .. })
        ));
}
#[test]
fn strip_invalid_images_malformed_data_uri_no_comma() {
    let url = "data:image/png;base64".to_string();
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
}
#[test]
fn strip_invalid_images_http_url_untouched() {
    let url = "https://example.com/photo.jpg".to_string();
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image {
                url: url.clone().into(),
            },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 0);
    assert!(matches!(
            &items[0],
            ConversationItem::User(u) if matches!(&u.content[0], ContentPart::Image { url: u } if u.as_ref() == "https://example.com/photo.jpg")
        ));
}
#[test]
fn strip_invalid_images_oversized_stripped() {
    use base64::Engine as _;
    let huge = vec![0u8; MAX_LOADED_IMAGE_BYTES + 1];
    let payload = base64::engine::general_purpose::STANDARD.encode(&huge);
    let url = format!("data:image/jpeg;base64,{payload}");
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
}
#[test]
fn strip_invalid_images_mixed_valid_and_invalid() {
    let valid_url = image_data_uri("image/png", &test_png_bytes());
    let invalid_url = "data:image/png;base64,!!!corrupt!!!".to_string();
    let http_url = "https://example.com/img.png".to_string();
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Text {
                text: "check these".into(),
            },
            ContentPart::Image {
                url: valid_url.clone().into(),
            },
            ContentPart::Image {
                url: invalid_url.into(),
            },
            ContentPart::Image {
                url: http_url.into(),
            },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
    if let ConversationItem::User(u) = &items[0] {
        assert_eq!(u.content.len(), 4);
        assert!(
                matches!(&u.content[0], ContentPart::Text { text } if text.as_ref() == "check these")
            );
        assert!(
                matches!(&u.content[1], ContentPart::Image { url } if url.as_ref() == valid_url.as_str())
            );
        assert!(
                matches!(&u.content[2], ContentPart::Text { text } if text.contains("invalid data"))
            );
        assert!(
                matches!(&u.content[3], ContentPart::Image { url } if url.as_ref() == "https://example.com/img.png")
            );
    } else {
        panic!("expected User");
    }
}
#[test]
fn strip_invalid_images_non_user_items_untouched() {
    let mut items = vec![
            ConversationItem::system("system prompt"),
            ConversationItem::assistant("response"),
            ConversationItem::tool_result("call_1", "result"),
        ];
    assert_eq!(strip_invalid_images(&mut items), 0);
    assert_eq!(items.len(), 3);
}
/// The read_file inline-attach shape: the poisoned
/// image lives in `ToolResultItem.images`, not in a user part. Invalid
/// entries are removed; valid ones survive.
#[test]
fn strip_invalid_images_heals_tool_result_images() {
    let mut png16 = Vec::new();
    image::ImageBuffer::from_pixel(16, 16, image::Rgba([9u8, 9, 9, 255]))
        .write_to(&mut std::io::Cursor::new(&mut png16), image::ImageFormat::Png)
        .unwrap();
    let bad_url = image_data_uri("image/png", &png16);
    let good_url = image_data_uri("image/png", &test_png_bytes());
    let mut items = vec![ConversationItem::tool_result_with_images(
            "call_1".to_string(),
            "Read image file: icon.png".to_string(),
            vec![
                ContentPart::Image {
                    url: good_url.clone().into(),
                },
                ContentPart::Image {
                    url: bad_url.into(),
                },
            ],
        )];
    assert_eq!(strip_invalid_images(&mut items), 1);
    let ConversationItem::ToolResult(t) = &items[0] else {
        panic!("expected ToolResult");
    };
    assert_eq!(t.images.len(), 1, "only the invalid image is removed");
    assert!(
            matches!(&t.images[0], ContentPart::Image { url } if url.as_ref() == good_url.as_str())
        );
}
#[test]
fn strip_invalid_images_empty_conversation() {
    let mut items: Vec<ConversationItem> = vec![];
    assert_eq!(strip_invalid_images(&mut items), 0);
}
#[test]
fn strip_invalid_images_empty_payload_stripped() {
    let url = "data:image/png;base64,".to_string();
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
}
#[test]
fn strip_invalid_images_case_insensitive_base64_marker() {
    use base64::Engine as _;
    let payload = base64::engine::general_purpose::STANDARD.encode(test_png_bytes());
    let url = format!("data:image/png;Base64,{payload}");
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 0);
    assert!(matches!(
            &items[0],
            ConversationItem::User(u) if matches!(&u.content[0], ContentPart::Image { .. })
        ));
}
/// Regression: a truncated JPEG persisted into history must be
/// stripped at load so resuming recovers.
#[test]
fn strip_invalid_images_truncated_jpeg_stripped() {
    let mut jpeg = test_jpeg_bytes();
    jpeg.truncate(jpeg.len() / 2);
    let url = image_data_uri("image/jpeg", &jpeg);
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Text {
                text: "[Image extracted from tool result above]".into(),
            },
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
    assert!(matches!(
            &items[0],
            ConversationItem::User(u)
                if matches!(&u.content[1], ContentPart::Text { text } if text.contains("invalid data"))
        ));
}
#[test]
fn strip_invalid_images_truncated_png_stripped() {
    let mut png = test_png_bytes();
    png.truncate(png.len() / 2);
    let url = image_data_uri("image/png", &png);
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
}
#[test]
fn strip_invalid_images_complete_jpeg_kept() {
    let url = image_data_uri("image/jpeg", &test_jpeg_bytes());
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 0);
}
/// Regression: a below-floor image persisted into history must be
/// stripped at load.
#[test]
fn strip_invalid_images_below_pixel_floor_stripped() {
    use image::{ImageBuffer, Rgba};
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(
        16,
        16,
        Rgba([10, 20, 30, 255]),
    );
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let url = image_data_uri("image/png", &png);
    let mut items = vec![ConversationItem::user_with_parts(vec![
            ContentPart::Image { url: url.into() },
        ])];
    assert_eq!(strip_invalid_images(&mut items), 1);
}
/// Write a chat_history.jsonl with the given lines into a fresh
/// session dir, then call `read_chat_history_sync` and return the
/// resulting `ConversationItem`s. Exercises the real on-read upgrade
/// path end-to-end (loader + serde + echo_agent_sampling_types::
/// upgrade_legacy_reasoning).
fn load_lines(lines: &[&str]) -> Vec<ConversationItem> {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    let chat_path = adapter.chat_file(&info);
    std::fs::create_dir_all(chat_path.parent().unwrap()).unwrap();
    std::fs::write(&chat_path, lines.join("\n") + "\n").unwrap();
    adapter.read_chat_history_sync(chat_path, CHAT_FORMAT_VERSION).unwrap()
}
/// Real-shape legacy fixture from a web-search session.
/// The assistant carries `reasoning: { text, encrypted, id }` inline —
/// the legacy echo-agent-build / Opus / chat-completions shape.
/// BackendToolCall sits as its own sibling line (it was already a
/// sibling variant in the legacy shape).
#[test]
fn read_chat_history_upgrades_legacy_singular_reasoning_to_sibling() {
    let items = load_lines(
        &[
            r#"{"type":"system","content":"You are helpful."}"#,
            r#"{"type":"user","content":[{"type":"text","text":"cats and dogs"}]}"#,
            r#"{"type":"backend_tool_call","kind":{"tool_type":"web_search","id":"ws_legacy_1","status":"completed","action":{"type":"search","query":"cats and dogs","sources":[]}}}"#,
            r#"{"type":"assistant","content":"results...","reasoning":{"text":"the results are about cats","encrypted":"enc-blob","id":"rs_legacy"},"model_id":"echo-agent-build"}"#,
        ],
    );
    assert_eq!(
            items.len(),
            5,
            "system + user + backend_tool_call + reconstructed reasoning + assistant"
        );
    match &items[3] {
        ConversationItem::Reasoning(r) => {
            assert_eq!(r.id, "rs_legacy");
            assert_eq!(r.encrypted_content.as_deref(), Some("enc-blob"));
            let echo_agent_sampling_types::rs::SummaryPart::SummaryText(s) = &r.summary[0];
            assert_eq!(s.text, "the results are about cats");
        }
        other => panic!("expected reconstructed Reasoning at index 3, got {other:?}"),
    }
    assert!(matches!(items[4], ConversationItem::Assistant(_)));
}
/// The `raw_output`-era shape: `raw_output: Vec<OutputItem>` on the assistant.
/// N parallel `tco_*` reasoning blobs survive as N sibling items, in
/// emission order, interleaved with backend tool calls.
#[test]
fn read_chat_history_upgrades_raw_output_parallel_tco_reasoning() {
    let lines = [
        r#"{"type":"system","content":"sys"}"#,
        r#"{"type":"user","content":[{"type":"text","text":"q"}]}"#,
        r#"{"type":"backend_tool_call","kind":{"tool_type":"web_search","id":"ws_1","status":"completed","action":{"type":"search","query":"q1","sources":[]}}}"#,
        r#"{"type":"backend_tool_call","kind":{"tool_type":"web_search","id":"ws_2","status":"completed","action":{"type":"search","query":"q2","sources":[]}}}"#,
        r#"{"type":"assistant","content":"answer","tool_calls":[],"raw_output":[{"type":"reasoning","id":"tco_1","summary":[],"encrypted_content":"e1"},{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"q1","sources":[]}},{"type":"reasoning","id":"tco_2","summary":[],"encrypted_content":"e2"},{"type":"web_search_call","id":"ws_2","status":"completed","action":{"type":"search","query":"q2","sources":[]}},{"type":"reasoning","id":"rs_main","summary":[{"type":"summary_text","text":"final"}]},{"type":"message","id":"m1","status":"completed","role":"assistant","content":[{"type":"output_text","text":"answer","annotations":[]}]}]}"#,
    ];
    let items = load_lines(&lines);
    let kinds: Vec<&'static str> = items
        .iter()
        .map(|i| match i {
            ConversationItem::System(_) => "system",
            ConversationItem::User(_) => "user",
            ConversationItem::Assistant(_) => "assistant",
            ConversationItem::ToolResult(_) => "tool_result",
            ConversationItem::BackendToolCall(_) => "backend_tool_call",
            ConversationItem::Reasoning(_) => "reasoning",
        })
        .collect();
    assert_eq!(
            kinds,
            vec![
                "system",
                "user",
                "backend_tool_call",
                "backend_tool_call",
                "reasoning",
                "reasoning",
                "reasoning",
                "assistant",
            ],
        );
    let reasoning_ids: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            ConversationItem::Reasoning(r) => Some(r.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning_ids, vec!["tco_1", "tco_2", "rs_main"]);
}
/// Hybrid file — legacy-shape turns at the front of the file, new-shape
/// turns appended at the back (the realistic shape when a user loads an
/// old session with a new binary and takes another turn). Verifies:
///
/// 1. The legacy turn's `reasoning` field is reconstructed as a sibling
///    *before* the legacy assistant.
/// 2. The post-PR sibling Reasoning row passes through unchanged and
///    lands before the post-PR assistant (no double-emission).
/// 3. `sibling_btc_ids_seen` correctly tracks ids across the boundary:
///    a BackendToolCall that appears as a sibling row in the post-PR
///    section does not get re-emitted by a (hypothetical) later legacy
///    assistant's raw_output that lists the same id.
/// 4. Final item order is a uniform sibling-shape `Vec<ConversationItem>`
///    that downstream code can replay without knowing about the seam.
#[test]
fn read_chat_history_handles_hybrid_legacy_and_post_pr_lines() {
    let items = load_lines(
        &[
            r#"{"type":"system","content":"sys"}"#,
            r#"{"type":"user","content":[{"type":"text","text":"q1"}]}"#,
            r#"{"type":"backend_tool_call","kind":{"tool_type":"web_search","id":"ws_legacy_1","status":"completed","action":{"type":"search","query":"q1","sources":[]}}}"#,
            r#"{"type":"assistant","content":"a1","reasoning":{"text":"legacy thinking","encrypted":"enc","id":"rs_legacy"},"model_id":"echo-agent-build"}"#,
            r#"{"type":"user","content":[{"type":"text","text":"q2"}]}"#,
            r#"{"type":"reasoning","id":"rs_postpr","summary":[{"type":"summary_text","text":"new thinking"}]}"#,
            r#"{"type":"backend_tool_call","kind":{"tool_type":"web_search","id":"ws_postpr","status":"completed","action":{"type":"search","query":"q2","sources":[]}}}"#,
            r#"{"type":"assistant","content":"a2","model_id":"echo-agent-build"}"#,
        ],
    );
    let kinds: Vec<&'static str> = items
        .iter()
        .map(|i| match i {
            ConversationItem::System(_) => "system",
            ConversationItem::User(_) => "user",
            ConversationItem::Assistant(_) => "assistant",
            ConversationItem::ToolResult(_) => "tool_result",
            ConversationItem::BackendToolCall(_) => "backend_tool_call",
            ConversationItem::Reasoning(_) => "reasoning",
        })
        .collect();
    assert_eq!(
            kinds,
            vec![
                // Turn 1 (legacy lifted)
                "system",
                "user",
                "backend_tool_call", // ws_legacy_1 (passthrough sibling row)
                "reasoning",         // reconstructed from assistant.reasoning
                "assistant",         // legacy assistant with legacy fields stripped
                // Turn 2 (post-PR passthrough)
                "user",
                "reasoning",         // passthrough sibling
                "backend_tool_call", // passthrough sibling
                "assistant",
            ],
            "hybrid file produces uniform sibling-shape output with no \
             cross-boundary corruption"
        );
    let reasoning_ids: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            ConversationItem::Reasoning(r) => Some(r.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning_ids, vec!["rs_legacy", "rs_postpr"]);
    let btc_ids: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            ConversationItem::BackendToolCall(b) => Some(b.id()),
            _ => None,
        })
        .collect();
    assert_eq!(btc_ids, vec!["ws_legacy_1", "ws_postpr"]);
    let ConversationItem::Assistant(legacy_assistant) = &items[4] else {
        panic!("expected legacy assistant at index 4");
    };
    assert_eq!(legacy_assistant.content.as_ref(), "a1");
    assert_eq!(
            legacy_assistant.model_id.as_deref(),
            Some("echo-agent-build"),
            "model_id preserved across the upgrade"
        );
    let ConversationItem::Reasoning(reconstructed) = &items[3] else {
        panic!("expected reconstructed Reasoning at index 3");
    };
    assert_eq!(reconstructed.id, "rs_legacy");
    assert_eq!(reconstructed.encrypted_content.as_deref(), Some("enc"));
    let echo_agent_sampling_types::rs::SummaryPart::SummaryText(s) = &reconstructed
        .summary[0];
    assert_eq!(s.text, "legacy thinking");
}
/// Already-new-shape sessions are unchanged by the loader.
/// Idempotent: re-loading the file produces the same items.
#[test]
fn read_chat_history_is_idempotent_on_post_pr_sessions() {
    let items = load_lines(
        &[
            r#"{"type":"system","content":"sys"}"#,
            r#"{"type":"user","content":[{"type":"text","text":"q"}]}"#,
            r#"{"type":"reasoning","id":"rs_x","summary":[{"type":"summary_text","text":"thought"}]}"#,
            r#"{"type":"assistant","content":"a","model_id":"echo-agent-build"}"#,
        ],
    );
    let kinds: Vec<&'static str> = items
        .iter()
        .map(|i| match i {
            ConversationItem::System(_) => "system",
            ConversationItem::User(_) => "user",
            ConversationItem::Assistant(_) => "assistant",
            ConversationItem::ToolResult(_) => "tool_result",
            ConversationItem::BackendToolCall(_) => "backend_tool_call",
            ConversationItem::Reasoning(_) => "reasoning",
        })
        .collect();
    assert_eq!(kinds, vec!["system", "user", "reasoning", "assistant"]);
}
/// Set up a session dir with a raw `chat_history.jsonl` and return
/// (adapter, chat path, loaded items).
fn load_raw_chat(
    temp_dir: &TempDir,
    raw: &[u8],
) -> (JsonlStorageAdapter, PathBuf, Vec<ConversationItem>) {
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    let chat_path = adapter.chat_file(&info);
    std::fs::create_dir_all(chat_path.parent().unwrap()).unwrap();
    std::fs::write(&chat_path, raw).unwrap();
    let items = adapter
        .read_chat_history_sync(chat_path.clone(), CHAT_FORMAT_VERSION)
        .unwrap();
    (adapter, chat_path, items)
}
fn user_text(items: &[ConversationItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|i| match i {
            ConversationItem::User(u) => {
                u.content
                    .iter()
                    .find_map(|p| match p {
                        ContentPart::Text { text } => Some(text.to_string()),
                        _ => None,
                    })
            }
            _ => None,
        })
        .collect()
}
/// A record torn mid-object (crash / ENOSPC mid-append) is skipped;
/// every other record loads, and the damaged original is quarantined
/// as `chat_history.jsonl.corrupt`.
#[test]
fn read_chat_history_skips_torn_line_and_quarantines_original() {
    let good_1 = r#"{"type":"user","content":[{"type":"text","text":"first"}]}"#;
    let torn = r#"{"type":"assistant","content":"partial answer that got cut off mid-wr"#;
    let good_2 = r#"{"type":"user","content":[{"type":"text","text":"second"}]}"#;
    let raw = format!("{good_1}\n{torn}\n{good_2}\n");
    let temp_dir = TempDir::new().unwrap();
    let (_, chat_path, items) = load_raw_chat(&temp_dir, raw.as_bytes());
    assert_eq!(
            user_text(&items),
            vec!["first", "second"],
            "records around the torn line must survive"
        );
    assert_eq!(items.len(), 2, "the torn record itself is dropped");
    let quarantine = chat_path.with_extension("jsonl.corrupt");
    assert_eq!(
            std::fs::read_to_string(&quarantine).unwrap(),
            raw,
            "original file must be preserved byte-for-byte for recovery"
        );
}
/// An image strip is destructive (re-persisted on spawn) and its
/// verdicts are client-side heuristics — so the pre-strip original must
/// be quarantined exactly like a torn-line load, keeping a false drop
/// recoverable.
#[test]
fn read_chat_history_quarantines_original_on_image_strip() {
    use base64::Engine as _;
    use image::{ImageBuffer, Rgba};
    let img: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::from_pixel(
        16,
        16,
        Rgba([9u8, 9, 9, 255]),
    );
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
    let line = format!(r#"{{"type":"user","content":[{{"type":"image","url":"{url}"}}]}}"#);
    let raw = format!("{line}\n");
    let temp_dir = TempDir::new().unwrap();
    let (_, chat_path, items) = load_raw_chat(&temp_dir, raw.as_bytes());
    assert!(matches!(
            &items[0],
            ConversationItem::User(u)
                if matches!(&u.content[0], ContentPart::Text { text } if text.contains("invalid data"))
        ));
    let quarantine = chat_path.with_extension("jsonl.corrupt");
    assert_eq!(
            std::fs::read_to_string(&quarantine).unwrap(),
            raw,
            "pre-strip original must be preserved for recovery"
        );
}
/// The pre-strip backup copies the live file once; the first copy wins
/// so a later strip cannot overwrite the earliest (fullest) backup.
#[tokio::test]
async fn backup_chat_history_before_strip_copies_once() {
    let original = r#"{"type":"user","content":[{"type":"text","text":"with image"}]}"#;
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    let chat_path = adapter.chat_file(&info);
    std::fs::create_dir_all(chat_path.parent().unwrap()).unwrap();
    std::fs::write(&chat_path, original).unwrap();
    adapter.backup_chat_history_before_strip(&info).await.unwrap();
    let backup = chat_path.with_extension("jsonl.pre-strip");
    assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            original,
            "backup must capture the pre-strip file"
        );
    std::fs::write(&chat_path, "stripped").unwrap();
    adapter.backup_chat_history_before_strip(&info).await.unwrap();
    assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            original,
            "first backup wins; a later strip must not overwrite it"
        );
}
/// The gate itself: when the backup cannot be written, the destructive
/// rewrite must not run: the live file keeps its images and the error
/// surfaces to the caller.
#[tokio::test]
async fn strip_rewrite_gated_skips_rewrite_when_backup_fails() {
    let original = r#"{"type":"user","content":[{"type":"text","text":"with image"}]}"#;
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    let chat_path = adapter.chat_file(&info);
    std::fs::create_dir_all(chat_path.parent().unwrap()).unwrap();
    std::fs::write(&chat_path, original).unwrap();
    std::fs::create_dir(chat_path.with_extension("jsonl.pre-strip.tmp")).unwrap();
    let result = crate::session::storage::strip_rewrite_gated(
            &adapter,
            &info,
            &[ConversationItem::user("stripped")],
        )
        .await;
    assert!(result.is_err(), "a failed backup must fail the strip");
    assert_eq!(
            std::fs::read_to_string(&chat_path).unwrap(),
            original,
            "the rewrite must not run when the backup failed"
        );
}
/// A missing chat file (fresh session, strip before first write) must
/// not error or create a backup.
#[tokio::test]
async fn backup_chat_history_before_strip_noops_without_file() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.backup_chat_history_before_strip(&info).await.unwrap();
    assert!(
            !adapter
                .chat_file(&info)
                .with_extension("jsonl.pre-strip")
                .exists()
        );
}
/// The exact incident shape: a partial record with the next record
/// appended straight onto it (no newline in between — the log-and-continue
/// append path pre-heal). The merged line fails with "expected `,` or `}`"
/// and is skipped; the load succeeds.
#[test]
fn read_chat_history_skips_merged_line_from_interrupted_append() {
    let good_1 = r#"{"type":"user","content":[{"type":"text","text":"kept"}]}"#;
    let partial = r#"{"type":"assistant","content":"cut mid-wri"#;
    let merged_onto = r#"{"type":"user","content":[{"type":"text","text":"lost"}]}"#;
    let good_2 = r#"{"type":"assistant","content":"after","model_id":"echo-agent-build"}"#;
    let raw = format!("{good_1}\n{partial}{merged_onto}\n{good_2}\n");
    let temp_dir = TempDir::new().unwrap();
    let (_, _, items) = load_raw_chat(&temp_dir, raw.as_bytes());
    assert_eq!(items.len(), 2, "merged line dropped, neighbors kept");
    assert!(matches!(&items[0], ConversationItem::User(_)));
    assert!(
            matches!(&items[1], ConversationItem::Assistant(a) if a.content.as_ref() == "after")
        );
}
/// A line torn in the middle of a multi-byte UTF-8 codepoint must poison
/// only itself — not the whole file (the old `read_to_string` failed the
/// entire load with InvalidData on any invalid UTF-8 byte).
#[test]
fn read_chat_history_skips_line_torn_mid_utf8_codepoint() {
    let good = r#"{"type":"user","content":[{"type":"text","text":"survives"}]}"#;
    let mut raw = Vec::new();
    raw.extend_from_slice(good.as_bytes());
    raw.push(b'\n');
    raw.extend_from_slice(br#"{"type":"assistant","content":"price: "#);
    raw.extend_from_slice(&[0xE2, 0x82]);
    raw.push(b'\n');
    let temp_dir = TempDir::new().unwrap();
    let (_, _, items) = load_raw_chat(&temp_dir, &raw);
    assert_eq!(user_text(&items), vec!["survives"]);
    assert_eq!(items.len(), 1);
}
/// Structurally valid JSON that decodes as neither ConversationItem nor
/// legacy ChatRequestMessage (schema drift / foreign writer) is skipped,
/// not fatal.
#[test]
fn read_chat_history_skips_undecodable_but_valid_json_line() {
    let good = r#"{"type":"user","content":[{"type":"text","text":"kept"}]}"#;
    let raw = format!("[1,2,3]\n{good}\n");
    let temp_dir = TempDir::new().unwrap();
    let (_, _, items) = load_raw_chat(&temp_dir, raw.as_bytes());
    assert_eq!(user_text(&items), vec!["kept"]);
    assert_eq!(items.len(), 1);
}
/// A record torn at EOF with no trailing newline (crash artifact before
/// any healing append ran) is skipped on read.
#[test]
fn read_chat_history_skips_torn_tail_without_trailing_newline() {
    let good = r#"{"type":"user","content":[{"type":"text","text":"kept"}]}"#;
    let raw = format!(r#"{good}{}"#, "\n{\"type\":\"assistant\",\"content\":\"cut");
    let temp_dir = TempDir::new().unwrap();
    let (_, _, items) = load_raw_chat(&temp_dir, raw.as_bytes());
    assert_eq!(user_text(&items), vec!["kept"]);
    assert_eq!(items.len(), 1);
}
/// First detection wins: a later read of a (differently) corrupt file
/// must not clobber the original quarantine evidence.
#[test]
fn read_chat_history_quarantine_preserves_first_evidence() {
    let good = r#"{"type":"user","content":[{"type":"text","text":"kept"}]}"#;
    let first_corruption = format!("{good}\n{{\"type\":\"assistant\",\"content\":\"v1-torn\n");
    let temp_dir = TempDir::new().unwrap();
    let (adapter, chat_path, _) = load_raw_chat(&temp_dir, first_corruption.as_bytes());
    let quarantine = chat_path.with_extension("jsonl.corrupt");
    assert_eq!(
            std::fs::read_to_string(&quarantine).unwrap(),
            first_corruption
        );
    let second_corruption = format!("{good}\n{{\"type\":\"assistant\",\"content\":\"v2-torn\n");
    std::fs::write(&chat_path, &second_corruption).unwrap();
    adapter.read_chat_history_sync(chat_path.clone(), CHAT_FORMAT_VERSION).unwrap();
    assert_eq!(
            std::fs::read_to_string(&quarantine).unwrap(),
            first_corruption,
            "earliest corruption evidence must be preserved"
        );
}
/// Invalid UTF-8 in `updates.jsonl` poisons only its own line.
#[tokio::test]
async fn read_updates_jsonl_skips_invalid_utf8_line() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let notification = SessionUpdate::Acp(
        Box::new(
            acp::SessionNotification::new(
                info.id.clone(),
                acp::SessionUpdate::UserMessageChunk(
                    acp::ContentChunk::new(
                        acp::ContentBlock::Text(acp::TextContent::new("hi".to_string())),
                    ),
                ),
            ),
        ),
    );
    adapter.append_update(&info, &notification).await.unwrap();
    let updates_path = adapter.updates_file(&info);
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&updates_path)
            .unwrap();
        f.write_all(&[0xE2, 0x82, b'\n']).unwrap();
    }
    let updates = adapter.read_updates_jsonl(updates_path).unwrap();
    assert_eq!(
            updates.len(),
            1,
            "valid line kept, invalid-UTF8 line skipped"
        );
}
/// A clean file must not leave a quarantine copy behind.
#[test]
fn read_chat_history_clean_file_writes_no_quarantine() {
    let good = r#"{"type":"user","content":[{"type":"text","text":"clean"}]}"#;
    let raw = format!("{good}\n");
    let temp_dir = TempDir::new().unwrap();
    let (_, chat_path, items) = load_raw_chat(&temp_dir, raw.as_bytes());
    assert_eq!(items.len(), 1);
    assert!(
            !chat_path.with_extension("jsonl.corrupt").exists(),
            "no corruption detected → no quarantine copy"
        );
}
/// Self-healing append: a torn trailing line (previous append crashed
/// mid-write, no trailing newline) is terminated before the new record is
/// written, so the new record lands on its own line and only the torn
/// record is lost on the next load.
#[tokio::test]
async fn append_chat_message_terminates_torn_trailing_line() {
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let chat_path = adapter.chat_file(&info);
    let good = r#"{"type":"user","content":[{"type":"text","text":"before crash"}]}"#;
    let torn = r#"{"type":"assistant","content":"cut mid-wr"#;
    std::fs::write(&chat_path, format!("{good}\n{torn}")).unwrap();
    adapter
        .append_chat_message(&info, &ConversationItem::user("after crash"))
        .await
        .unwrap();
    let raw = std::fs::read_to_string(&chat_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert_eq!(
            lines.len(),
            3,
            "good + torn(terminated) + appended: {raw:?}"
        );
    assert_eq!(lines[1], torn, "torn record isolated on its own line");
    assert!(
            lines[2].contains("after crash"),
            "new record on a fresh line: {:?}",
            lines[2]
        );
    let items = adapter.read_chat_history_sync(chat_path, CHAT_FORMAT_VERSION).unwrap();
    assert_eq!(user_text(&items), vec!["before crash", "after crash"]);
}
/// Appending to a healthy file must not inject spurious blank lines.
#[tokio::test]
async fn append_chat_message_no_spurious_newlines_on_clean_tail() {
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    adapter.append_chat_message(&info, &ConversationItem::user("one")).await.unwrap();
    adapter.append_chat_message(&info, &ConversationItem::user("two")).await.unwrap();
    let raw = std::fs::read_to_string(adapter.chat_file(&info)).unwrap();
    assert_eq!(raw.lines().count(), 2);
    assert!(!raw.contains("\n\n"), "no blank lines injected: {raw:?}");
    let items = adapter
        .read_chat_history_sync(adapter.chat_file(&info), CHAT_FORMAT_VERSION)
        .unwrap();
    assert_eq!(user_text(&items), vec!["one", "two"]);
}
#[tokio::test]
async fn retry_after_lost_ack_converges_memory_and_disk_to_authoritative_item() {
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let storage = std::sync::Arc::new(adapter.clone());
    let actor_info = info.clone();
    tokio::spawn(async move {
        let mut drop_first_ack = true;
        while let Some(
            crate::session::persistence::PersistenceMsg::AppendCwdSwitchAndAck {
                item,
                respond_to,
            },
        ) = rx.recv().await
        {
            let result = storage
                .append_cwd_switch_commit_aware(&actor_info, &item)
                .await
                .map_err(|error| match error {
                    crate::session::storage::AppendCwdSwitchError::NotCommitted(
                        error,
                    ) => echo_agent_chat_state::StrictAppendError::NotCommitted(error),
                    crate::session::storage::AppendCwdSwitchError::Committed {
                        acknowledgement,
                        source,
                    } => {
                        echo_agent_chat_state::StrictAppendError::Committed {
                            acknowledgement,
                            source,
                        }
                    }
                });
            if drop_first_ack {
                drop_first_ack = false;
            } else {
                let _ = respond_to.send(result);
            }
        }
    });
    let persistence = crate::session::chat_persistence::ChannelChatPersistence::new(tx);
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let chat = echo_agent_chat_state::ChatStateActor::spawn(
        vec![],
        echo_agent_sampling_types::SamplingConfig {
            base_url: String::new(),
            model: String::new(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: Default::default(),
            extra_headers: Default::default(),
            query_params: Default::default(),
            env_http_headers: Default::default(),
            context_window: std::num::NonZeroU64::new(128_000).unwrap(),
            reasoning_effort: None,
            stream_tool_calls: None,
        },
        Box::new(persistence),
        event_tx,
        tokio_util::sync::CancellationToken::new(),
    );
    assert!(matches!(
            chat.append_working_directory_switch_and_ack(
                "authoritative A".into(),
                std::num::NonZeroU64::new(5).unwrap(),
            )
            .await,
            Err(echo_agent_chat_state::StrictAppendError::Indeterminate(_))
        ));
    assert!(chat.get_conversation().await.is_empty());
    assert!(matches!(
            chat.append_working_directory_switch_and_ack(
                "candidate B".into(),
                std::num::NonZeroU64::new(5).unwrap(),
            )
            .await
            .unwrap(),
            echo_agent_chat_state::StrictAppendAck::AlreadyPresent(item)
                if item.text_content() == "authoritative A"
        ));
    let memory = chat.get_conversation().await;
    let disk = adapter
        .read_chat_history_sync(adapter.chat_file(&info), CHAT_FORMAT_VERSION)
        .unwrap();
    assert_eq!(memory.len(), 1);
    assert_eq!(disk.len(), 1);
    assert_eq!(memory[0].text_content(), "authoritative A");
    assert_eq!(disk[0].text_content(), "authoritative A");
}
#[tokio::test]
async fn acknowledged_chat_append_preserves_existing_file_bytes_and_appends_once() {
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let first = ConversationItem::system("sys");
    let second = ConversationItem::assistant("answer");
    adapter.append_chat_message(&info, &first).await.unwrap();
    adapter.append_chat_message(&info, &second).await.unwrap();
    let path = adapter.chat_file(&info);
    let prefix = std::fs::read(&path).unwrap();
    let switch = ConversationItem::working_directory_switch("moved", 4);
    assert!(matches!(
            adapter
                .append_cwd_switch_commit_aware(&info, &switch)
                .await
                .unwrap(),
            echo_agent_chat_state::StrictAppendAck::Appended
        ));
    let after = std::fs::read(&path).unwrap();
    assert!(after.starts_with(&prefix));
    let mut expected_suffix = serde_json::to_vec(&switch).unwrap();
    expected_suffix.push(b'\n');
    assert_eq!(&after[prefix.len()..], expected_suffix);
    let loaded = adapter.read_chat_history_sync(path, CHAT_FORMAT_VERSION).unwrap();
    assert_eq!(loaded.len(), 3);
    assert_eq!(loaded[2].working_directory_switch_generation(), Some(4));
}
/// Same self-healing for `updates.jsonl` appends, and the lenient reader
/// skips the isolated torn line.
#[tokio::test]
async fn append_update_terminates_torn_trailing_line() {
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let notification = |text: &str| SessionUpdate::Acp(
        Box::new(
            acp::SessionNotification::new(
                info.id.clone(),
                acp::SessionUpdate::UserMessageChunk(
                    acp::ContentChunk::new(
                        acp::ContentBlock::Text(acp::TextContent::new(text.to_string())),
                    ),
                ),
            ),
        ),
    );
    adapter.append_update(&info, &notification("first")).await.unwrap();
    let updates_path = adapter.updates_file(&info);
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&updates_path)
            .unwrap();
        f.write_all(br#"{"timestamp":"2026-07-06T00:00:00Z","update":{"sessionId":"tor"#)
            .unwrap();
    }
    adapter.append_update(&info, &notification("second")).await.unwrap();
    let raw = std::fs::read_to_string(&updates_path).unwrap();
    assert_eq!(
            raw.lines().count(),
            3,
            "first + torn(terminated) + second: {raw:?}"
        );
    let updates = adapter.read_updates_jsonl(updates_path).unwrap();
    assert_eq!(updates.len(), 2, "torn line skipped, real updates kept");
}
/// End-to-end resume-path regression for the incident: a live session
/// whose `chat_history.jsonl` contains a merged record (crash mid-append,
/// then log-and-continue appended the next record onto the partial line)
/// must still load via `load_session_without_updates` — previously this
/// returned InvalidData ("expected `,` or `}` at line 1 column N"),
/// surfacing to the user as "Couldn't load session: … FS_OTHER" and
/// permanently bricking the session.
#[tokio::test]
async fn load_session_without_updates_survives_merged_chat_line() {
    let temp_dir = TempDir::new().unwrap();
    let info = create_test_info();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    adapter.init_session(&info, default_model_id()).await.unwrap();
    adapter
        .append_chat_message(&info, &ConversationItem::user("real turn"))
        .await
        .unwrap();
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(adapter.chat_file(&info))
            .unwrap();
        f.write_all(
                br#"{"type":"assistant","content":"cut mid{"type":"user","content":[{"type":"text","text":"merged"}]}"#,
            )
            .unwrap();
        f.write_all(b"\n").unwrap();
    }
    let loaded = adapter.load_session_without_updates(&info).await.unwrap();
    assert_eq!(
            user_text(&loaded.chat_history),
            vec!["real turn"],
            "resume succeeds; only the merged record is dropped"
        );
}
#[cfg(unix)]
use crate::test_support::{set_unix_mode, unix_mode};
#[tokio::test]
#[cfg(unix)]
async fn init_session_creates_owner_only_session_and_parent_dirs() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let session_dir = temp_dir
        .path()
        .join("sessions")
        .join(crate::util::echo_agent_home::encode_cwd_dirname(&info.cwd))
        .join(info.id.to_string());
    assert_eq!(unix_mode(&session_dir), 0o700, "session dir must be 0700");
    assert_eq!(
            unix_mode(session_dir.parent().unwrap()),
            0o700,
            "<encoded-cwd> parent must be 0700"
        );
    assert_eq!(
            unix_mode(&temp_dir.path().join("sessions")),
            0o700,
            "sessions root must be 0700"
        );
}
/// Dirs loosened on disk (e.g. by an older echoagent) re-tighten on next touch.
#[tokio::test]
#[cfg(unix)]
async fn init_session_retightens_loosened_existing_dirs() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let info = create_test_info();
    adapter.init_session(&info, default_model_id()).await.unwrap();
    let session_dir = temp_dir
        .path()
        .join("sessions")
        .join(crate::util::echo_agent_home::encode_cwd_dirname(&info.cwd))
        .join(info.id.to_string());
    set_unix_mode(&session_dir, 0o755);
    set_unix_mode(session_dir.parent().unwrap(), 0o755);
    adapter.init_session(&info, default_model_id()).await.unwrap();
    assert_eq!(unix_mode(&session_dir), 0o700);
    assert_eq!(unix_mode(session_dir.parent().unwrap()), 0o700);
}
#[tokio::test]
#[cfg(unix)]
async fn copy_session_data_creates_owner_only_target_dir() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let source = create_test_info();
    adapter.init_session(&source, default_model_id()).await.unwrap();
    let target = Info {
        id: acp::SessionId::new("forked-session-456"),
        cwd: source.cwd.clone(),
    };
    adapter
        .copy_session_data(&source, &target, CopySessionOptions::default())
        .await
        .unwrap();
    let target_dir = temp_dir
        .path()
        .join("sessions")
        .join(crate::util::echo_agent_home::encode_cwd_dirname(&target.cwd))
        .join(target.id.to_string());
    assert_eq!(unix_mode(&target_dir), 0o700);
}
/// Explicit-mode parents are caller-owned (temp roots in tests): never chmod'd.
#[tokio::test]
#[cfg(unix)]
async fn explicit_session_dir_does_not_tighten_parent() {
    let temp_dir = TempDir::new().unwrap();
    let parent = temp_dir.path().join("caller-owned");
    std::fs::create_dir(&parent).unwrap();
    set_unix_mode(&parent, 0o755);
    let child = parent.join("child-session");
    let adapter = JsonlStorageAdapter::with_explicit_session_dir(child.clone());
    adapter.init_session(&create_test_info(), default_model_id()).await.unwrap();
    assert_eq!(unix_mode(&child), 0o700, "explicit dir must be tightened");
    assert_eq!(
            unix_mode(&parent),
            0o755,
            "caller-owned parent must not be chmod'd in Explicit mode"
        );
}
