use super::support::create_test_actor;

use crate::extensions::notification::{
    CompactionCheckpointFile, CompactionCheckpointInfo,
    SessionNotification as EchoAgentNotification, SessionUpdate as EchoAgentSessionUpdate,
};
use crate::sampling::ConversationItem;
use crate::session::storage::{SessionUpdate, SessionUpdateEnvelope};
use crate::session::{RewindMode, RewindRequest};
use agent_client_protocol as acp;

fn user_chunk(text: &str, prompt_index: usize) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new("s"),
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

fn agent_chunk(text: &str) -> SessionUpdate {
    SessionUpdate::Acp(Box::new(acp::SessionNotification::new(
        acp::SessionId::new("s"),
        acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::Text(
            acp::TextContent::new(text.to_string()),
        ))),
    )))
}

fn checkpoint_update(id: &str, prompt_index_at_compaction: usize) -> SessionUpdate {
    SessionUpdate::EchoAgent(Box::new(EchoAgentNotification {
        session_id: acp::SessionId::new("s"),
        update: EchoAgentSessionUpdate::CompactionCheckpoint(Box::new(CompactionCheckpointInfo {
            checkpoint_id: id.to_string(),
            prompt_index_at_compaction,
            checkpoint_file: format!("compaction_checkpoints/{id}.json"),
            auto_continue: None,
            schema_version: 1,
            created_at: "2026-01-01T00:00:00Z".to_string(),
        })),
        meta: None,
    }))
}

/// Writes the shared cross-compaction fixture into `session_dir`: a checkpoint
/// file (compacted `[SYS, SUMMARY]` at prompt 5) plus an `updates.jsonl` with
/// prompts P0..P6 and the checkpoint record between P4 and P5.
fn write_compacted_session_fixture(session_dir: &std::path::Path, ckpt_id: &str) {
    std::fs::create_dir_all(session_dir.join("compaction_checkpoints")).unwrap();

    let ckpt_file = CompactionCheckpointFile {
        checkpoint_id: ckpt_id.to_string(),
        prompt_index_at_compaction: 5,
        compacted_history: vec![
            ConversationItem::system("SYS"),
            ConversationItem::user("SUMMARY"),
        ],
        schema_version: 1,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        original_user_info: Some("UI0".to_string()),
        reread_file_paths: vec![],
    };
    std::fs::write(
        session_dir.join(format!("compaction_checkpoints/{ckpt_id}.json")),
        serde_json::to_vec(&ckpt_file).unwrap(),
    )
    .unwrap();

    let updates = vec![
        user_chunk("P0", 0),
        user_chunk("P1", 1),
        user_chunk("P2", 2),
        user_chunk("P3", 3),
        user_chunk("P4", 4),
        checkpoint_update(ckpt_id, 5),
        user_chunk("P5", 5),
        agent_chunk("R5"),
        user_chunk("P6", 6),
    ];
    let mut content = Vec::new();
    for u in &updates {
        let env = SessionUpdateEnvelope::from_update(u).unwrap();
        content.extend(serde_json::to_vec(&env).unwrap());
        content.push(b'\n');
    }
    std::fs::write(session_dir.join("updates.jsonl"), content).unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn rewind_pre_compaction_with_cancelled_turns_truncates_context_gb2961() {
    let local = tokio::task::LocalSet::new();
    local.run_until(run_rewind_scenario()).await;
}

async fn run_rewind_scenario() {
    let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    actor.session_info.id = acp::SessionId::new(format!("rw-e2e-{unique}"));

    let session_dir = crate::session::persistence::session_dir(&actor.session_info);
    write_compacted_session_fixture(&session_dir, "ckpt5");

    let mut snap = actor
        .chat_state_handle
        .snapshot()
        .await
        .expect("snapshot available");
    snap.conversation = vec![
        ConversationItem::system("SYS"),
        ConversationItem::user("UI1"),
        ConversationItem::user("SUMMARY"),
        ConversationItem::user("P5"),
        ConversationItem::assistant("R5"),
        ConversationItem::user("P6"),
    ];
    snap.prompt_index = 7;
    snap.prompt_texts = (0..7).map(|i| format!("P{i}")).collect();
    snap.last_compaction_prompt_index = Some(5);
    actor.chat_state_handle.restore_snapshot(snap);

    let resp = actor
        .handle_rewind(RewindRequest {
            target_prompt_index: 3,
            force: true,
            file_guard: None,
            mode: RewindMode::ConversationOnly,
        })
        .await
        .expect("handle_rewind ok");
    assert!(resp.success, "rewind should succeed: {resp:?}");

    let conv = actor.chat_state_handle.get_conversation().await;
    let texts: Vec<String> = conv.iter().map(|c| c.text_content()).collect();

    let _ = std::fs::remove_dir_all(&session_dir);

    assert_eq!(
        texts,
        vec!["SYS", "UI0", "P0", "P1", "P2"],
        "conversation must truncate to prompts 0..2 (got {texts:?})"
    );
    assert!(
        !texts
            .iter()
            .any(|t| ["P3", "P4", "P5", "P6", "SUMMARY"].contains(&t.as_str())),
        "post-target prompts / compacted summary must not leak into context: {texts:?}"
    );
    assert_eq!(
        actor.chat_state_handle.get_prompt_index().await,
        3,
        "prompt_index must be reset to the rewind target"
    );
}

/// `FilesOnly` is exempt from the chat-state prompt-index bound (its real bound
/// is the on-disk snapshot index), so it no-ops to success when out of range —
/// the property the bridge relies on when the chat-state index is empty.
/// `ConversationOnly` is NOT exempt and still rejects an out-of-range target.
#[tokio::test(flavor = "current_thread")]
async fn files_only_rewind_is_exempt_from_chat_state_bound() {
    let local = tokio::task::LocalSet::new();
    local.run_until(run_files_only_bound_scenario()).await;
}

async fn run_files_only_bound_scenario() {
    let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

    let mut snap = actor
        .chat_state_handle
        .snapshot()
        .await
        .expect("snapshot available");
    snap.prompt_index = 2;
    snap.prompt_texts = vec!["P0".into(), "P1".into()];
    actor.chat_state_handle.restore_snapshot(snap);

    // Out-of-range FilesOnly: exempt → reverts nothing (no snapshots) but
    // succeeds.
    let oor = actor
        .handle_rewind(RewindRequest {
            target_prompt_index: 5,
            force: true,
            file_guard: None,
            mode: RewindMode::FilesOnly,
        })
        .await
        .expect("files-only rewind ok");
    assert!(
        oor.success,
        "out-of-range FilesOnly must no-op succeed: {oor:?}"
    );
    assert!(oor.reverted_files.is_empty());

    // In-range FilesOnly also succeeds.
    let in_range = actor
        .handle_rewind(RewindRequest {
            target_prompt_index: 1,
            force: true,
            file_guard: None,
            mode: RewindMode::FilesOnly,
        })
        .await
        .expect("files-only rewind ok");
    assert!(
        in_range.success,
        "in-range FilesOnly must succeed: {in_range:?}"
    );

    // ConversationOnly is still bounded by the chat-state index.
    let convo = actor
        .handle_rewind(RewindRequest {
            target_prompt_index: 5,
            force: true,
            file_guard: None,
            mode: RewindMode::ConversationOnly,
        })
        .await
        .expect("handle_rewind returns Ok(success=false)");
    assert!(
        !convo.success,
        "out-of-range ConversationOnly must be rejected"
    );
    assert!(convo.error.is_some());
}

/// `rewind_file_counts` (the `GetRewindFileCounts` actor arm) maps the
/// file-state tracker's per-prompt snapshot metadata to `prompt_index → count`.
#[tokio::test(flavor = "current_thread")]
async fn rewind_file_counts_maps_snapshot_metadata() {
    let local = tokio::task::LocalSet::new();
    local.run_until(run_file_counts_scenario()).await;
}

async fn run_file_counts_scenario() {
    use std::path::Path;

    let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

    let cwd = Path::new("/tmp");
    // Prompt 0 has two distinct file snapshots; prompt 1 has one.
    actor
        .file_state_tracker
        .add_before_snapshot_for_prompt(0, Path::new("/tmp/a.rs"), cwd, Some("a".into()))
        .await;
    actor
        .file_state_tracker
        .add_before_snapshot_for_prompt(0, Path::new("/tmp/b.rs"), cwd, Some("b".into()))
        .await;
    actor
        .file_state_tracker
        .add_before_snapshot_for_prompt(1, Path::new("/tmp/c.rs"), cwd, Some("c".into()))
        .await;

    let counts = actor.rewind_file_counts().await;
    assert_eq!(counts.get(&0).copied(), Some(2));
    assert_eq!(counts.get(&1).copied(), Some(1));
    assert_eq!(counts.get(&2).copied(), None);
}

/// A cross-compaction rewind to BEFORE the compaction point rebuilds the
/// conversation without a summary, so the stale `last_compaction_prompt_index`
/// must be cleared — otherwise the per-model `x-compactions-remaining` header
/// would wrongly report `0` for a session that no longer holds a summary.
#[tokio::test(flavor = "current_thread")]
async fn rewind_before_compaction_clears_stale_compaction_marker() {
    let local = tokio::task::LocalSet::new();
    local.run_until(run_clears_marker_scenario()).await;
}

async fn run_clears_marker_scenario() {
    use echo_agent_sampling_types::CompactionsRemaining;
    let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    actor.session_info.id = acp::SessionId::new(format!("rw-marker-{unique}"));

    let session_dir = crate::session::persistence::session_dir(&actor.session_info);
    write_compacted_session_fixture(&session_dir, "ckptm");

    let mut snap = actor
        .chat_state_handle
        .snapshot()
        .await
        .expect("snapshot available");
    snap.conversation = vec![
        ConversationItem::system("SYS"),
        ConversationItem::user("UI1"),
        ConversationItem::user("SUMMARY"),
        ConversationItem::user("P5"),
        ConversationItem::assistant("R5"),
        ConversationItem::user("P6"),
    ];
    snap.prompt_index = 7;
    snap.prompt_texts = (0..7).map(|i| format!("P{i}")).collect();
    // The session believes it holds a compaction summary from prompt 5.
    snap.last_compaction_prompt_index = Some(5);
    actor.chat_state_handle.restore_snapshot(snap);

    // Rewind to prompt 3 — before the compaction point (5), so the summary is
    // dropped from the rebuilt conversation and the marker must be cleared.
    let resp = actor
        .handle_rewind(RewindRequest {
            target_prompt_index: 3,
            force: true,
            file_guard: None,
            mode: RewindMode::ConversationOnly,
        })
        .await
        .expect("handle_rewind ok");
    assert!(resp.success, "rewind should succeed: {resp:?}");

    let marker = actor
        .chat_state_handle
        .get_last_compaction_prompt_index()
        .await;

    // End-to-end: advertise support so the gate runs, then read the header
    // off the reconstructed config — it must report a fresh "1", not stale "0".
    actor
        .compactions_remaining
        .set(Some(CompactionsRemaining::Dynamic(true)));
    let header = actor
        .reconstruct_full_config()
        .await
        .extra_headers
        .get("x-compactions-remaining")
        .cloned();

    let _ = std::fs::remove_dir_all(&session_dir);

    assert_eq!(
        marker, None,
        "pre-compaction rewind must clear the stale compaction marker"
    );
    assert_eq!(
        header.as_deref(),
        Some("1"),
        "header must report 1 after the summary is dropped (got {header:?})"
    );
}

/// Forking a session must carry the `compaction_checkpoints/{uuid}.json` files
/// along with the copied checkpoint records — replay hard-requires each
/// referenced file, so without the copy every rewind in the forked session
/// fails with "compaction checkpoint file missing". Drives the production
/// `fork_session` path so this test tracks its copy wiring.
#[tokio::test(flavor = "current_thread")]
async fn rewind_succeeds_in_forked_session_with_compaction_checkpoint() {
    let local = tokio::task::LocalSet::new();
    local.run_until(run_forked_rewind_scenario()).await;
}

async fn run_forked_rewind_scenario() {
    use crate::session::fork::{ForkSessionRequest, fork_session};
    use crate::session::storage::{JsonlStorageAdapter, StorageAdapter};

    let (gateway_tx, _gateway_rx) = tokio::sync::mpsc::unbounded_channel();
    let (persistence_tx, _persistence_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut actor = create_test_actor(0, 200_000, 80, gateway_tx, persistence_tx).await;

    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut source_info = actor.session_info.clone();
    source_info.id = acp::SessionId::new(format!("rw-fork-src-{unique}"));
    let fork_id = format!("rw-fork-dst-{unique}");
    actor.session_info.id = acp::SessionId::new(fork_id.clone());

    // fork_session reads the source summary, so init a real session first.
    JsonlStorageAdapter::with_root(crate::util::echo_agent_home::echo_agent_home())
        .init_session(
            &source_info,
            crate::session::persistence::default_model_id(),
        )
        .await
        .unwrap();
    let source_dir = crate::session::persistence::session_dir(&source_info);
    write_compacted_session_fixture(&source_dir, "ckptf");

    fork_session(
        ForkSessionRequest {
            source_session_id: source_info.id.to_string(),
            source_cwd: source_info.cwd.clone(),
            new_cwd: actor.session_info.cwd.clone(),
            new_session_id: Some(fork_id.clone()),
            ..Default::default()
        },
        "test-agent",
        None,
    )
    .await
    .expect("fork_session ok");

    let target_dir = crate::session::persistence::session_dir(&actor.session_info);
    let forked_checkpoint = target_dir.join("compaction_checkpoints/ckptf.json");

    // Simulate the forked session's live post-compaction state.
    let mut snap = actor
        .chat_state_handle
        .snapshot()
        .await
        .expect("snapshot available");
    snap.conversation = vec![
        ConversationItem::system("SYS"),
        ConversationItem::user("UI1"),
        ConversationItem::user("SUMMARY"),
        ConversationItem::user("P5"),
        ConversationItem::assistant("R5"),
        ConversationItem::user("P6"),
    ];
    snap.prompt_index = 7;
    snap.prompt_texts = (0..7).map(|i| format!("P{i}")).collect();
    snap.last_compaction_prompt_index = Some(5);
    actor.chat_state_handle.restore_snapshot(snap);

    // Rewind to a post-compaction target: replay must load the checkpoint
    // file from the FORKED session dir.
    let resp = actor
        .handle_rewind(RewindRequest {
            target_prompt_index: 6,
            force: true,
            file_guard: None,
            mode: RewindMode::ConversationOnly,
        })
        .await
        .expect("handle_rewind ok");

    let checkpoint_copied = forked_checkpoint.is_file();
    let prompt_index = actor.chat_state_handle.get_prompt_index().await;

    let _ = std::fs::remove_dir_all(&source_dir);
    let _ = std::fs::remove_dir_all(&target_dir);

    assert!(
        checkpoint_copied,
        "fork must copy the referenced checkpoint file"
    );
    assert!(
        resp.success,
        "rewind in a forked session must succeed once checkpoint files are copied: {resp:?}"
    );
    assert_eq!(
        prompt_index, 6,
        "prompt_index must be reset to the rewind target"
    );
}
