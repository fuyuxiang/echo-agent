use crate::sampling::ConversationItem;
use crate::session::info::Info;
use crate::session::persistence::{CHAT_FORMAT_VERSION, default_model_id};
use crate::session::storage::{
    CopySessionOptions, JsonlStorageAdapter, SessionUpdate, StorageAdapter,
};
use crate::tools::todo::TodoState;
use agent_client_protocol as acp;
use tempfile::TempDir;

fn fork_user_chunk(session_id: &str, text: &str, prompt_index: usize) -> SessionUpdate {
    let chunk = acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
        text.to_string(),
    )))
    .meta(
        serde_json::json!({ "promptIndex": prompt_index })
            .as_object()
            .cloned(),
    );
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new(session_id),
        acp::SessionUpdate::UserMessageChunk(chunk),
    )))
}

fn fork_agent_chunk(session_id: &str, text: &str) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new(session_id),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text.to_string()),
        ))),
    )))
}

fn fork_rewind_marker(session_id: &str, target_prompt_index: usize) -> SessionUpdate {
    use crate::extensions::notification::{
        SessionNotification as EchoAgentSessionNotification,
        SessionUpdate as EchoAgentSessionUpdateType,
    };
    SessionUpdate::EchoAgent(Box::new(EchoAgentSessionNotification {
        session_id: acp::SessionId::new(session_id),
        update: EchoAgentSessionUpdateType::RewindMarker {
            target_prompt_index,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        },
        meta: None,
    }))
}

fn chat_user(text: &str, prompt_index: usize) -> ConversationItem {
    let mut item = ConversationItem::user(text);
    item.set_prompt_index(prompt_index);
    item
}

/// Fork truncation targets the live branch (dead-branch runs from a
/// prior rewind overlap its stamps, since indices are branch-local) and keeps
/// prompt N inclusive in both the updates and chat (model-context) files.
#[tokio::test]
async fn copy_session_data_fork_truncates_live_branch_inclusive() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let sid = "src-rewound";
    let source_info = Info {
        id: acp::SessionId::new(sid),
        cwd: "/src".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    // Prompt 1 was rewound and retried: P1-dead/A1-dead is the dead branch.
    for update in [
        fork_user_chunk(sid, "P0", 0),
        fork_agent_chunk(sid, "A0"),
        fork_user_chunk(sid, "P1-dead", 1),
        fork_agent_chunk(sid, "A1-dead"),
        fork_rewind_marker(sid, 1),
        fork_user_chunk(sid, "P1b", 1),
        fork_agent_chunk(sid, "A1b"),
        fork_user_chunk(sid, "P2", 2),
    ] {
        adapter.append_update(&source_info, &update).await.unwrap();
    }
    for item in [
        chat_user("P0", 0),
        ConversationItem::assistant("A0"),
        chat_user("P1b", 1),
        ConversationItem::assistant("A1b"),
        chat_user("P2", 2),
    ] {
        adapter
            .append_chat_message(&source_info, &item)
            .await
            .unwrap();
    }

    let fork_at = |target: usize, fork_id: &str| {
        let target_info = Info {
            id: acp::SessionId::new(fork_id),
            cwd: "/src".to_string(),
        };
        let options = CopySessionOptions {
            target_prompt_index: Some(target),
            ..Default::default()
        };
        (target_info, options)
    };

    // Fork at live prompt 1: keeps P0, A0, P1b, A1b in both files. A raw
    // run count would cut inside the dead branch instead.
    let (target_info, options) = fork_at(1, "fork-at-1");
    let result = adapter
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();
    assert_eq!(result.updates_copied, 4);
    assert_eq!(result.chat_messages_copied, 4);
    let loaded = adapter.load_session(&target_info).await.unwrap();
    let last = loaded.updates.last().unwrap();
    assert!(
        matches!(
            last,
            SessionUpdate::Acp(n) if matches!(
                &n.update,
                acp::SessionUpdate::AgentMessageChunk(c)
                    if matches!(&c.content, acp::ContentBlock::Text(t) if t.text == "A1b")
            )
        ),
        "fork must end at the live branch's A1b, got {last:?}"
    );

    // Prompt 0 is kept inclusive; an exclusive cut would copy an empty
    // model context here.
    let (target_info, options) = fork_at(0, "fork-at-0");
    let result = adapter
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();
    assert_eq!(result.updates_copied, 2, "P0 + A0");
    assert_eq!(result.chat_messages_copied, 2, "P0 + A0 in model context");
}

/// Without a `target_prompt_index`, every line streams through: rewind
/// markers and dead branches survive a plain fork. A regression that routes
/// the default path through the rewind filter would strip them silently.
#[tokio::test]
async fn copy_session_data_without_prompt_target_preserves_dead_branches() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let sid = "src-dead-branch";
    let source_info = Info {
        id: acp::SessionId::new(sid),
        cwd: "/src".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    for update in [
        fork_user_chunk(sid, "P0", 0),
        fork_agent_chunk(sid, "A0"),
        fork_user_chunk(sid, "P1-dead", 1),
        fork_rewind_marker(sid, 1),
        fork_user_chunk(sid, "P1b", 1),
    ] {
        adapter.append_update(&source_info, &update).await.unwrap();
    }

    let target_info = Info {
        id: acp::SessionId::new("fork-plain"),
        cwd: "/src".to_string(),
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, CopySessionOptions::default())
        .await
        .unwrap();
    assert_eq!(
        result.updates_copied, 5,
        "dead branch and rewind marker must survive a plain fork"
    );
}

/// The streaming fork copy skips torn or undecodable lines like the load
/// path does, both with and without a prompt-index cut.
#[tokio::test]
async fn copy_session_data_skips_torn_updates_lines() {
    use std::io::Write as _;

    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let sid = "src-torn";
    let source_info = Info {
        id: acp::SessionId::new(sid),
        cwd: "/src".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    for update in [fork_user_chunk(sid, "P0", 0), fork_agent_chunk(sid, "A0")] {
        adapter.append_update(&source_info, &update).await.unwrap();
    }
    // A torn append (truncated JSON) and an undecodable line.
    let updates_path = adapter.updates_file_path(&source_info).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&updates_path)
        .unwrap();
    file.write_all(b"{\"method\":\"session/update\",\"params\":{tor\n")
        .unwrap();
    file.write_all(&[0xFF, 0xFE, b'\n']).unwrap();
    drop(file);
    adapter
        .append_update(&source_info, &fork_user_chunk(sid, "P1", 1))
        .await
        .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("fork-torn"),
        cwd: "/src".to_string(),
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, CopySessionOptions::default())
        .await
        .unwrap();
    assert_eq!(result.updates_copied, 3, "P0 + A0 + P1, torn lines dropped");
    let loaded = adapter.load_session(&target_info).await.unwrap();
    assert_eq!(loaded.updates.len(), 3);

    let target_info = Info {
        id: acp::SessionId::new("fork-torn-at-0"),
        cwd: "/src".to_string(),
    };
    let options = CopySessionOptions {
        target_prompt_index: Some(0),
        ..Default::default()
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();
    assert_eq!(result.updates_copied, 2, "P0 + A0; torn tail and P1 cut");
}

/// A torn line inside a multi-chunk user run ends the run during the prompt
/// cut, so the second chunk opens a new counted turn, matching replay's
/// raw-line semantics. Pins the boundary so a classifier change is deliberate.
#[tokio::test]
async fn torn_line_inside_user_run_splits_the_run_for_prompt_cut() {
    use std::io::Write as _;

    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());
    let sid = "src-torn-mid-run";
    let source_info = Info {
        id: acp::SessionId::new(sid),
        cwd: "/src".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    for update in [
        fork_user_chunk(sid, "P0", 0),
        fork_agent_chunk(sid, "A0"),
        fork_user_chunk(sid, "P1a", 1),
    ] {
        adapter.append_update(&source_info, &update).await.unwrap();
    }
    let updates_path = adapter.updates_file_path(&source_info).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&updates_path)
        .unwrap();
    file.write_all(b"{torn mid-run\n").unwrap();
    drop(file);
    for update in [
        fork_user_chunk(sid, "P1b", 1),
        fork_agent_chunk(sid, "A1"),
        fork_user_chunk(sid, "P2", 2),
    ] {
        adapter.append_update(&source_info, &update).await.unwrap();
    }

    let target_info = Info {
        id: acp::SessionId::new("fork-torn-mid-run"),
        cwd: "/src".to_string(),
    };
    let options = CopySessionOptions {
        target_prompt_index: Some(1),
        ..Default::default()
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();
    // P1b re-counts as a turn after the torn split, so the cut lands before
    // it: P0, A0, P1a survive. The contiguous-run cut would have kept 5.
    assert_eq!(result.updates_copied, 3, "P0 + A0 + P1a");
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
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new("Test response".to_string()),
        ))),
    )
}

fn create_test_plan_state() -> TodoState {
    TodoState::default()
}

#[tokio::test]
async fn copy_session_data_copies_compaction_segments_when_enabled() {
    use crate::extensions::notification::CompactionSegmentFile;
    use echo_agent_sampling_types::ConversationItem;

    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("seg-src"),
        cwd: "/source/workspace".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    for msg in &create_test_chat_messages() {
        adapter
            .append_chat_message(&source_info, msg)
            .await
            .unwrap();
    }

    // Two compaction segments → compaction/{segment_000.md, segment_001.md, INDEX.md}.
    let seg = |s: &str| CompactionSegmentFile {
        items: vec![ConversationItem::user("a"), ConversationItem::user("b")],
        summary: s.to_string(),
        detail: echo_agent_chat_state::CompactionDetail::Verbose,
        timestamp: "2026-01-01T00:00:00Z".to_string(),
    };
    adapter
        .write_compaction_segment(&source_info, &seg("first"))
        .await
        .unwrap();
    adapter
        .write_compaction_segment(&source_info, &seg("second"))
        .await
        .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("seg-dst"),
        cwd: "/target/workspace".to_string(),
    };
    let result = adapter
        .copy_session_data(
            &source_info,
            &target_info,
            CopySessionOptions {
                copy_compaction_segments: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(result.compaction_segments_copied, 3); // 2 segments + INDEX.md

    let dst = adapter
        .session_dir(&target_info)
        .join(echo_agent_compaction_transcript::COMPACTION_DIR);
    assert!(dst.join("segment_000.md").is_file());
    assert!(dst.join("segment_001.md").is_file());
    assert!(dst.join("INDEX.md").is_file());
    assert!(
        std::fs::read_to_string(dst.join("segment_000.md"))
            .unwrap()
            .contains("# HISTORICAL -- DO NOT EDIT")
    );

    let target2 = Info {
        id: acp::SessionId::new("seg-dst-default"),
        cwd: "/target2/workspace".to_string(),
    };
    let result2 = adapter
        .copy_session_data(&source_info, &target2, CopySessionOptions::default())
        .await
        .unwrap();
    assert_eq!(result2.compaction_segments_copied, 0);
    assert!(
        !adapter
            .session_dir(&target2)
            .join(echo_agent_compaction_transcript::COMPACTION_DIR)
            .exists()
    );
}

/// A `compaction_checkpoint` record pointing at `compaction_checkpoints/{id}.json`.
fn checkpoint_record(id: &str) -> SessionUpdate {
    checkpoint_record_with_path(id, &format!("compaction_checkpoints/{id}.json"))
}

/// A `compaction_checkpoint` record with an arbitrary `checkpoint_file` path.
fn checkpoint_record_with_path(id: &str, checkpoint_file: &str) -> SessionUpdate {
    use crate::extensions::notification::{
        CompactionCheckpointInfo, SessionNotification as EchoAgentSessionNotification,
        SessionUpdate as EchoAgentSessionUpdateType,
    };
    SessionUpdate::EchoAgent(Box::new(EchoAgentSessionNotification {
        session_id: acp::SessionId::new("ckpt-src"),
        update: EchoAgentSessionUpdateType::CompactionCheckpoint(Box::new(
            CompactionCheckpointInfo {
                checkpoint_id: id.to_string(),
                prompt_index_at_compaction: 1,
                checkpoint_file: checkpoint_file.to_string(),
                auto_continue: None,
                schema_version: 1,
                created_at: "2026-01-01T00:00:00Z".to_string(),
            },
        )),
        meta: None,
    }))
}

/// A user message chunk stamped with `_meta.promptIndex` so
/// `truncate_for_prompt_by` counts it as a turn.
fn prompt_user_chunk(text: &str, prompt_index: usize) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new("ckpt-src"),
        acp::SessionUpdate::UserMessageChunk(
            acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(
                text.to_string(),
            )))
            .meta(
                serde_json::json!({ "promptIndex": prompt_index })
                    .as_object()
                    .cloned(),
            ),
        ),
    )))
}

async fn write_checkpoint_file(adapter: &JsonlStorageAdapter, info: &Info, id: &str) {
    use crate::extensions::notification::CompactionCheckpointFile;
    adapter
        .write_compaction_checkpoint(
            info,
            &CompactionCheckpointFile {
                checkpoint_id: id.to_string(),
                prompt_index_at_compaction: 1,
                compacted_history: vec![],
                schema_version: 1,
                created_at: "2026-01-01T00:00:00Z".to_string(),
                original_user_info: None,
                reread_file_paths: vec![],
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn copy_session_data_copies_referenced_compaction_checkpoints() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("ckpt-src"),
        cwd: "/source/workspace".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    // Two records referencing the same file (e.g. a chained fork) must
    // still produce one copy.
    for _ in 0..2 {
        adapter
            .append_update(&source_info, &checkpoint_record("ckpt-a"))
            .await
            .unwrap();
    }
    write_checkpoint_file(&adapter, &source_info, "ckpt-a").await;

    let target_info = Info {
        id: acp::SessionId::new("ckpt-dst"),
        cwd: "/target/workspace".to_string(),
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, CopySessionOptions::default())
        .await
        .unwrap();

    assert_eq!(result.compaction_checkpoints_copied, 1);
    assert_eq!(
        result.updates_copied, 2,
        "checkpoint records must be copied"
    );
    let rel = "compaction_checkpoints/ckpt-a.json";
    let copied = std::fs::read(adapter.session_dir(&target_info).join(rel)).unwrap();
    let original = std::fs::read(adapter.session_dir(&source_info).join(rel)).unwrap();
    assert_eq!(copied, original, "checkpoint file must be copied verbatim");
}

#[tokio::test]
async fn fork_filter_copy_skips_compaction_checkpoints() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("ckpt-src"),
        cwd: "/source/workspace".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    adapter
        .append_update(&source_info, &checkpoint_record("ckpt-a"))
        .await
        .unwrap();
    write_checkpoint_file(&adapter, &source_info, "ckpt-a").await;

    let target_info = Info {
        id: acp::SessionId::new("ckpt-dst"),
        cwd: "/target/workspace".to_string(),
    };
    // fork_filter clears the copied updates, so no record survives and no
    // checkpoint file should come along.
    let result = adapter
        .copy_session_data(
            &source_info,
            &target_info,
            CopySessionOptions {
                fork_filter: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(result.compaction_checkpoints_copied, 0);
    assert_eq!(
        result.updates_copied, 0,
        "fork_filter clears the transcript"
    );
    assert!(
        !adapter
            .session_dir(&target_info)
            .join("compaction_checkpoints")
            .exists()
    );
}

#[tokio::test]
async fn target_prompt_index_truncation_gates_checkpoint_copy() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("ckpt-src"),
        cwd: "/source/workspace".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    for update in [
        prompt_user_chunk("P0", 0),
        checkpoint_record("ckpt-early"),
        prompt_user_chunk("P1", 1),
        prompt_user_chunk("P2", 2),
        checkpoint_record("ckpt-late"),
    ] {
        adapter.append_update(&source_info, &update).await.unwrap();
    }
    write_checkpoint_file(&adapter, &source_info, "ckpt-early").await;
    write_checkpoint_file(&adapter, &source_info, "ckpt-late").await;

    let target_info = Info {
        id: acp::SessionId::new("ckpt-dst"),
        cwd: "/target/workspace".to_string(),
    };
    // Truncating to prompt 0 keeps [P0, ckpt-early] and drops the rest.
    let result = adapter
        .copy_session_data(
            &source_info,
            &target_info,
            CopySessionOptions {
                target_prompt_index: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(result.compaction_checkpoints_copied, 1);
    let dst = adapter
        .session_dir(&target_info)
        .join("compaction_checkpoints");
    assert!(
        dst.join("ckpt-early.json").is_file(),
        "record before the cut keeps its checkpoint file"
    );
    assert!(
        !dst.join("ckpt-late.json").exists(),
        "record after the cut must not pull its checkpoint file"
    );
}

#[tokio::test]
async fn dangling_checkpoint_record_copies_without_file() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("ckpt-src"),
        cwd: "/source/workspace".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    // Record present but its file was never written (already-broken source).
    adapter
        .append_update(&source_info, &checkpoint_record("ckpt-gone"))
        .await
        .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("ckpt-dst"),
        cwd: "/target/workspace".to_string(),
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, CopySessionOptions::default())
        .await
        .unwrap();

    assert_eq!(result.compaction_checkpoints_copied, 0);
    assert_eq!(result.updates_copied, 1, "the record itself still copies");
    assert!(
        !adapter
            .session_dir(&target_info)
            .join("compaction_checkpoints/ckpt-gone.json")
            .exists()
    );
}

#[tokio::test]
async fn checkpoint_record_with_non_checkpoint_path_is_not_copied() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("ckpt-src"),
        cwd: "/source/workspace".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    // A doctored record addressing another session file: copying it would
    // clobber the target's rewritten updates.jsonl with raw source bytes.
    adapter
        .append_update(
            &source_info,
            &checkpoint_record_with_path("ckpt-evil", "updates.jsonl"),
        )
        .await
        .unwrap();
    // Real checkpoint dir present so the path-shape guard (not the
    // missing-dir guard) is what rejects the record.
    std::fs::create_dir_all(
        adapter
            .session_dir(&source_info)
            .join("compaction_checkpoints"),
    )
    .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("ckpt-dst"),
        cwd: "/target/workspace".to_string(),
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, CopySessionOptions::default())
        .await
        .unwrap();

    assert_eq!(result.compaction_checkpoints_copied, 0);
    // The target updates must keep the transformed record (session id
    // rewritten to the fork), not the source file's raw bytes.
    let loaded = adapter.load_session(&target_info).await.unwrap();
    assert_eq!(loaded.updates.len(), 1);
    match &loaded.updates[0] {
        SessionUpdate::EchoAgent(notification) => {
            assert_eq!(notification.session_id.0.as_ref(), "ckpt-dst");
        }
        other => panic!("Expected EchoAgent update, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_checkpoint_file_is_not_copied() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("ckpt-src"),
        cwd: "/source/workspace".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    adapter
        .append_update(&source_info, &checkpoint_record("ckpt-a"))
        .await
        .unwrap();
    // Plant a symlink where the checkpoint file should be: the copy must
    // not follow it out of the session directory.
    let ckpt_dir = adapter
        .session_dir(&source_info)
        .join("compaction_checkpoints");
    std::fs::create_dir_all(&ckpt_dir).unwrap();
    let outside = temp_dir.path().join("outside.json");
    std::fs::write(&outside, b"outside bytes").unwrap();
    std::os::unix::fs::symlink(&outside, ckpt_dir.join("ckpt-a.json")).unwrap();

    let target_info = Info {
        id: acp::SessionId::new("ckpt-dst"),
        cwd: "/target/workspace".to_string(),
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, CopySessionOptions::default())
        .await
        .unwrap();

    assert_eq!(result.compaction_checkpoints_copied, 0);
    assert!(
        !adapter
            .session_dir(&target_info)
            .join("compaction_checkpoints/ckpt-a.json")
            .exists()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlinked_checkpoint_dir_is_not_copied() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("ckpt-src"),
        cwd: "/source/workspace".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    adapter
        .append_update(&source_info, &checkpoint_record("ckpt-a"))
        .await
        .unwrap();
    // Plant the whole compaction_checkpoints dir as a symlink to an
    // outside dir holding a matching .json: nothing may be copied.
    let outside_dir = temp_dir.path().join("outside");
    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("ckpt-a.json"), b"outside bytes").unwrap();
    std::os::unix::fs::symlink(
        &outside_dir,
        adapter
            .session_dir(&source_info)
            .join("compaction_checkpoints"),
    )
    .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("ckpt-dst"),
        cwd: "/target/workspace".to_string(),
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, CopySessionOptions::default())
        .await
        .unwrap();

    assert_eq!(result.compaction_checkpoints_copied, 0);
    assert!(
        !adapter
            .session_dir(&target_info)
            .join("compaction_checkpoints")
            .exists()
    );
}

#[tokio::test]
async fn copy_session_data_basic() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("source-session-123"),
        cwd: "/source/workspace".to_string(),
    };

    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    let messages = create_test_chat_messages();
    for msg in &messages {
        adapter
            .append_chat_message(&source_info, msg)
            .await
            .unwrap();
    }

    let notification = create_test_notification();
    adapter
        .append_update(&source_info, &SessionUpdate::Acp(Box::new(notification)))
        .await
        .unwrap();

    let plan_state = create_test_plan_state();
    adapter
        .write_plan_state(&source_info, &plan_state)
        .await
        .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("fork-source-session-123-abcd1234"),
        cwd: "/target/workspace".to_string(),
    };

    let options = CopySessionOptions {
        parent_session_id: Some("source-session-123".to_string()),
        new_model_id: None,
        target_prompt_index: None,
        ..Default::default()
    };
    let result = adapter
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();

    assert_eq!(result.chat_messages_copied, 3);
    assert_eq!(result.updates_copied, 1);
    assert!(result.plan_state_copied);

    let loaded = adapter.load_session(&target_info).await.unwrap();
    assert_eq!(loaded.summary.info.id, target_info.id);
    assert_eq!(loaded.summary.info.cwd, "/target/workspace");
    assert_eq!(loaded.summary.session_kind.as_deref(), Some("fork"));
    assert_eq!(
        loaded.summary.parent_session_id,
        Some("source-session-123".to_string())
    );
    assert!(loaded.summary.forked_at.is_some());
    assert_eq!(loaded.chat_history.len(), 3);
    assert_eq!(loaded.updates.len(), 1);
    match &loaded.updates[0] {
        SessionUpdate::Acp(notification) => {
            assert_eq!(
                notification.session_id.0.as_ref(),
                "fork-source-session-123-abcd1234"
            );
        }
        _ => panic!("Expected ACP update"),
    }
    assert!(loaded.plan_state.is_some());
}

#[tokio::test]
async fn copy_session_data_without_plan() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("source-no-plan"),
        cwd: "/source/workspace".to_string(),
    };

    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    adapter
        .append_chat_message(&source_info, &ConversationItem::user("Hello"))
        .await
        .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("fork-source-no-plan-12345678"),
        cwd: "/target/workspace".to_string(),
    };

    let result = adapter
        .copy_session_data(&source_info, &target_info, Default::default())
        .await
        .unwrap();

    assert_eq!(result.chat_messages_copied, 1);
    assert_eq!(result.updates_copied, 0);
    assert!(!result.plan_state_copied);

    let loaded = adapter.load_session(&target_info).await.unwrap();
    assert!(loaded.plan_state.is_none());
}

#[tokio::test]
async fn copy_session_data_transforms_echo_agent_updates() {
    use crate::extensions::notification::{
        DiffContent, SessionNotification as EchoAgentSessionNotification,
        SessionUpdate as EchoAgentSessionUpdateType,
    };

    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("source-echo-agent"),
        cwd: "/source".to_string(),
    };

    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    let echo_agent_notification = EchoAgentSessionNotification {
        session_id: acp::SessionId::new("source-echo-agent"),
        update: EchoAgentSessionUpdateType::DiffReview {
            content: vec![DiffContent {
                diff: acp::Diff::new(std::path::PathBuf::from("/test/file.rs"), "new".to_string())
                    .old_text(Some("old".to_string())),
            }],
        },
        meta: None,
    };
    adapter
        .append_update(
            &source_info,
            &SessionUpdate::EchoAgent(Box::new(echo_agent_notification)),
        )
        .await
        .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("fork-source-echo-agent-abcd1234"),
        cwd: "/target".to_string(),
    };

    adapter
        .copy_session_data(&source_info, &target_info, Default::default())
        .await
        .unwrap();

    let loaded = adapter.load_session(&target_info).await.unwrap();
    match &loaded.updates[0] {
        SessionUpdate::EchoAgent(notification) => {
            assert_eq!(
                notification.session_id.0.as_ref(),
                "fork-source-echo-agent-abcd1234"
            );
        }
        _ => panic!("Expected EchoAgent update"),
    }
}

#[tokio::test]
async fn copy_session_data_source_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("nonexistent"),
        cwd: "/nonexistent".to_string(),
    };

    let target_info = Info {
        id: acp::SessionId::new("fork-nonexistent-abcd1234"),
        cwd: "/target".to_string(),
    };

    let result = adapter
        .copy_session_data(&source_info, &target_info, Default::default())
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn copy_session_data_with_model_override() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("source-model-test"),
        cwd: "/source".to_string(),
    };

    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("fork-model-test"),
        cwd: "/target".to_string(),
    };

    let options = CopySessionOptions {
        parent_session_id: Some("source-model-test".to_string()),
        new_model_id: Some("echo-agent-3".to_string()),
        target_prompt_index: None,
        ..Default::default()
    };
    adapter
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();

    let loaded = adapter.load_session(&target_info).await.unwrap();
    assert_eq!(loaded.summary.current_model_id.0.as_ref(), "echo-agent-3");
    assert_eq!(
        loaded.summary.parent_session_id,
        Some("source-model-test".to_string())
    );
}

#[tokio::test]
async fn copy_session_data_skips_tool_state_directory() {
    let temp_dir = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(temp_dir.path().to_path_buf());

    let source_info = Info {
        id: acp::SessionId::new("source-dir-tool-state"),
        cwd: "/source/project".to_string(),
    };

    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    adapter
        .append_chat_message(&source_info, &ConversationItem::user("Hello"))
        .await
        .unwrap();

    let source_dir = adapter.session_dir(&source_info);
    std::fs::create_dir_all(source_dir.join("tool_state.json").join("terminal")).unwrap();

    let target_info = Info {
        id: acp::SessionId::new("fork-dir-tool-state"),
        cwd: "/target/worktree".to_string(),
    };

    let result = adapter
        .copy_session_data(&source_info, &target_info, Default::default())
        .await
        .unwrap();

    assert!(!result.tool_state_copied);
    assert!(
        !adapter
            .session_dir(&target_info)
            .join("tool_state.json")
            .is_file()
    );
}

#[tokio::test]
async fn copy_fork_provenance_persisted_in_summary() {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let source_info = Info {
        id: acp::SessionId::new("src-prov"),
        cwd: "/src".to_string(),
    };
    let target_info = Info {
        id: acp::SessionId::new("tgt-prov"),
        cwd: "/tgt".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();

    let options = CopySessionOptions {
        parent_session_id: Some("src-prov".to_string()),
        session_kind: Some("subagent_fork".to_string()),
        fork_context_source: Some("forked".to_string()),
        fork_parent_prompt_id: Some("prompt-42".to_string()),
        ..Default::default()
    };
    adapter
        .copy_session_data(&source_info, &target_info, options)
        .await
        .unwrap();

    let data = adapter.load_session(&target_info).await.unwrap();
    assert_eq!(data.summary.session_kind.as_deref(), Some("subagent_fork"));
    assert_eq!(data.summary.fork_context_source.as_deref(), Some("forked"));
    assert_eq!(
        data.summary.fork_parent_prompt_id.as_deref(),
        Some("prompt-42")
    );
}

#[tokio::test]
async fn copy_session_data_inherits_source_summary_fields() {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let source_info = Info {
        id: acp::SessionId::new("src-inherit"),
        cwd: "/src".to_string(),
    };
    adapter
        .init_session(&source_info, default_model_id())
        .await
        .unwrap();
    adapter
        .update_git_head(
            &source_info,
            Some("abc123".into()),
            Some("feature-branch".into()),
        )
        .await
        .unwrap();
    // Set the profile on disk so the assertion is independent of the
    // process-global configured profile.
    let mut src_summary = adapter.read_summary_sync(&source_info).unwrap();
    src_summary.sandbox_profile = Some("workspace".to_string());
    adapter
        .write_summary_sync(&source_info, &src_summary)
        .unwrap();

    let target_info = Info {
        id: acp::SessionId::new("tgt-inherit"),
        cwd: "/tgt".to_string(),
    };
    adapter
        .copy_session_data(&source_info, &target_info, CopySessionOptions::default())
        .await
        .unwrap();

    let loaded = adapter.load_summary(&target_info).await.unwrap();
    assert_eq!(loaded.head_commit.as_deref(), Some("abc123"));
    assert_eq!(loaded.head_branch.as_deref(), Some("feature-branch"));
    assert_eq!(loaded.sandbox_profile.as_deref(), Some("workspace"));
}

async fn assert_copy_clears_pending_relocation(fork_filter: bool) {
    use crate::session::persistence::PendingCwdSwitchReminder;

    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let source = Info {
        id: acp::SessionId::new(format!("pending-source-{fork_filter}")),
        cwd: "/src".into(),
    };
    let target = Info {
        id: acp::SessionId::new(format!("pending-target-{fork_filter}")),
        cwd: "/target".into(),
    };
    let mut summary = adapter
        .init_session(&source, default_model_id())
        .await
        .unwrap();
    summary.cwd_generation = 3;
    summary.previous_cwd = Some("/older".into());
    summary.pending_cwd_switch_reminder = Some(PendingCwdSwitchReminder {
        cwd_generation: 3,
        previous_cwd: "/src".into(),
        destination_cwd: "/destination".into(),
        content: "switch".into(),
        destination_project_instructions: None,
    });
    adapter.write_summary_sync(&source, &summary).unwrap();
    adapter
        .append_chat_message(
            &source,
            &ConversationItem::working_directory_switch("switch", 3),
        )
        .await
        .unwrap();

    adapter
        .copy_session_data(
            &source,
            &target,
            CopySessionOptions {
                fork_filter,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let copied = adapter.read_summary_sync(&target).unwrap();
    assert_eq!(copied.cwd_generation, 3);
    assert_eq!(copied.previous_cwd.as_deref(), Some("/older"));
    assert!(copied.pending_cwd_switch_reminder.is_none());
    let expected_generation = if fork_filter { 0 } else { 3 };
    assert_eq!(
        copied.cwd_switch_bookkeeping_generation,
        expected_generation
    );
    if !fork_filter {
        let before = copied.num_chat_messages;
        assert!(matches!(
            adapter
                .append_cwd_switch_commit_aware(
                    &target,
                    &ConversationItem::working_directory_switch("switch", 3),
                )
                .await
                .unwrap(),
            echo_agent_chat_state::StrictAppendAck::AlreadyPresent(item)
                if item.text_content() == "switch"
        ));
        let retried = adapter.read_summary_sync(&target).unwrap();
        assert_eq!(retried.num_chat_messages, before);
        assert_eq!(
            adapter
                .read_chat_history_sync(adapter.chat_file(&target), CHAT_FORMAT_VERSION)
                .unwrap()
                .iter()
                .filter(|item| item.working_directory_switch_generation() == Some(3))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn unfiltered_copy_clears_pending_relocation() {
    assert_copy_clears_pending_relocation(false).await;
}

#[tokio::test]
async fn filtered_copy_clears_pending_relocation() {
    assert_copy_clears_pending_relocation(true).await;
}

/// Each sidecar flag gates exactly its own file: one fork per flag disables
/// only that flag and asserts only its file is missing, so a transposed flag
/// or path in the `copy_sidecar_file` call sites fails. A defaults fork then
/// proves all five copy with their contents intact.
#[tokio::test]
async fn sidecar_flags_gate_their_files_independently() {
    let tmp = TempDir::new().unwrap();
    let adapter = JsonlStorageAdapter::with_root(tmp.path().to_path_buf());
    let source = Info {
        id: acp::SessionId::new("src-sidecars"),
        cwd: "/src".to_string(),
    };
    adapter
        .init_session(&source, default_model_id())
        .await
        .unwrap();
    std::fs::write(adapter.plan_file(&source), b"plan").unwrap();
    std::fs::write(adapter.signals_file(&source), b"signals").unwrap();
    std::fs::write(adapter.plan_mode_state_file(&source), b"plan-mode").unwrap();
    std::fs::write(
        adapter.session_dir(&source).join("tool_state.json"),
        b"{\"todo\":[]}",
    )
    .unwrap();
    std::fs::write(adapter.announcement_state_file(&source), b"announcements").unwrap();

    type DisableFlag = fn(&mut CopySessionOptions);
    let cases: [(&str, DisableFlag); 5] = [
        ("plan", |o| o.copy_plan_state = false),
        ("signals", |o| o.copy_signals = false),
        ("plan_mode", |o| o.copy_plan_mode_state = false),
        ("tool_state", |o| o.copy_tool_state = false),
        ("announcement", |o| o.copy_announcement_state = false),
    ];
    for (off, (name, disable)) in cases.iter().enumerate() {
        let target = Info {
            id: acp::SessionId::new(format!("tgt-sidecar-off-{name}")),
            cwd: "/tgt".to_string(),
        };
        let mut options = CopySessionOptions::default();
        disable(&mut options);
        let result = adapter
            .copy_session_data(&source, &target, options)
            .await
            .unwrap();
        let copied = [
            result.plan_state_copied,
            result.signals_copied,
            result.plan_mode_state_copied,
            result.tool_state_copied,
            result.announcement_state_copied,
        ];
        let present = [
            adapter.plan_file(&target).exists(),
            adapter.signals_file(&target).exists(),
            adapter.plan_mode_state_file(&target).exists(),
            adapter
                .session_dir(&target)
                .join("tool_state.json")
                .exists(),
            adapter.announcement_state_file(&target).exists(),
        ];
        for (i, (copied, present)) in copied.into_iter().zip(present).enumerate() {
            let expected = i != off;
            assert_eq!(copied, expected, "{name} off: sidecar {i} copied flag");
            assert_eq!(present, expected, "{name} off: sidecar {i} file present");
        }
    }

    let target_on = Info {
        id: acp::SessionId::new("tgt-sidecars-on"),
        cwd: "/tgt".to_string(),
    };
    let result = adapter
        .copy_session_data(&source, &target_on, CopySessionOptions::default())
        .await
        .unwrap();
    assert!(result.plan_state_copied);
    assert!(result.signals_copied);
    assert!(result.plan_mode_state_copied);
    assert!(result.tool_state_copied);
    assert!(result.announcement_state_copied);
    assert_eq!(
        std::fs::read(adapter.plan_file(&target_on)).unwrap(),
        b"plan"
    );
    assert_eq!(
        std::fs::read(adapter.signals_file(&target_on)).unwrap(),
        b"signals"
    );
    assert_eq!(
        std::fs::read(adapter.plan_mode_state_file(&target_on)).unwrap(),
        b"plan-mode"
    );
    assert_eq!(
        std::fs::read(adapter.session_dir(&target_on).join("tool_state.json")).unwrap(),
        b"{\"todo\":[]}"
    );
    assert_eq!(
        std::fs::read(adapter.announcement_state_file(&target_on)).unwrap(),
        b"announcements"
    );
}

/// Boundary matrix for the capped line reader: exactly-cap content is kept,
/// cap-plus-one is discarded without consuming an index (so the two copy
/// passes stay aligned), a drain spanning several read chunks terminates, and
/// an unterminated within-cap tail is kept.
#[test]
fn capped_line_reader_discards_overlong_lines_without_shifting_indexes() {
    fn collect(input: &[u8], cap: usize) -> Vec<(usize, Vec<u8>)> {
        let mut seen = Vec::new();
        super::for_each_jsonl_line_capped(std::io::Cursor::new(input), cap, |index, line| {
            seen.push((index, line.to_vec()));
            Ok(std::ops::ControlFlow::Continue(()))
        })
        .unwrap();
        seen
    }

    // Exactly cap content bytes: kept.
    assert_eq!(collect(b"abcd\n", 4), vec![(0, b"abcd".to_vec())]);
    // One over cap: discarded; the next line takes the next index, not a
    // shifted one.
    assert_eq!(
        collect(b"aa\nxxxxx\nbb\n", 4),
        vec![(0, b"aa".to_vec()), (1, b"bb".to_vec())]
    );
    // Overlong spanning several drain chunks still finds the line end.
    assert_eq!(
        collect(b"xxxxxxxxxxxxxxxxxxxxx\ncc\n", 4),
        vec![(0, b"cc".to_vec())]
    );
    // Overlong unterminated at EOF: drain hits EOF and stops cleanly.
    assert_eq!(collect(b"aa\nxxxxxxxx", 4), vec![(0, b"aa".to_vec())]);
    // Unterminated within-cap tail is kept, matching the uncapped reader.
    assert_eq!(
        collect(b"aa\nbb", 4),
        vec![(0, b"aa".to_vec()), (1, b"bb".to_vec())]
    );
}
