//! Non-ignored guard for the testkit's core path: synthesize a small session
//! with [`synth::prepare_session`], then confirm the production replay reader
//! parses every persisted update back with the right per-kind counts.
//!
//! `load_updates_for_replay_at` is the typed reader and keeps every update
//! (only `EchoAgent` updates are dropped); the redundant-ACU skip is a later
//! line-based step in the client replay path, not asserted here.

use agent_client_protocol as acp;
use tempfile::TempDir;

use echo_agent_runtime::session::storage::{
    JsonlStorageAdapter, StorageAdapter, load_updates_for_replay_at,
};
use echo_agent_runtime::session::testkit::synth::{self, SessionSpec};

#[tokio::test]
async fn synth_replay_roundtrip_parses_every_persisted_update() {
    let root = TempDir::new().unwrap();
    let cwd = TempDir::new().unwrap();
    // Distinct per-turn counts so a miscount of any kind is unambiguous.
    let spec = SessionSpec {
        turns: 3,
        acu_per_turn: 2,
        catalog_commands: 2,
        catalog_desc_len: 8,
        agent_chunks_per_turn: 4,
        agent_chunk_len: 16,
        rewind_points: 0,
        files_per_rewind: 0,
        file_content_len: 0,
    };

    let (info, _dir) = synth::prepare_session(root.path(), cwd.path(), &spec).await;

    let replayed = load_updates_for_replay_at(info.id.0.as_ref(), root.path())
        .expect("load_updates_for_replay_at")
        .unwrap_or_default();

    let count = |pred: fn(&acp::SessionUpdate) -> bool| replayed.iter().filter(|u| pred(u)).count();
    let users = count(|u| matches!(u, acp::SessionUpdate::UserMessageChunk(_)));
    let acus = count(|u| matches!(u, acp::SessionUpdate::AvailableCommandsUpdate(_)));
    let agents = count(|u| matches!(u, acp::SessionUpdate::AgentMessageChunk(_)));

    assert_eq!(users, spec.turns, "one user chunk per turn");
    assert_eq!(
        acus,
        spec.turns * spec.acu_per_turn,
        "every ACU is preserved"
    );
    assert_eq!(
        agents,
        spec.turns * spec.agent_chunks_per_turn,
        "every agent chunk is preserved"
    );
    assert_eq!(
        replayed.len(),
        spec.turns * (1 + spec.acu_per_turn + spec.agent_chunks_per_turn),
        "no update is dropped or duplicated by the typed replay reader"
    );
}

/// The adapter-driven bench generator reaches its byte target and emits updates
/// the production reader parses. Uses the blocking wrapper because this is a
/// synchronous `#[test]` with no ambient runtime.
#[test]
fn make_session_with_size_reaches_target_and_parses() {
    let root = TempDir::new().unwrap();
    let target: u64 = 8 * 1024;

    let info = synth::make_session_with_size_blocking(root.path(), target);

    let adapter = JsonlStorageAdapter::with_root(root.path().to_path_buf());
    let updates_path = adapter.updates_file_path(&info).expect("updates path");
    let len = std::fs::metadata(&updates_path)
        .expect("stat updates.jsonl")
        .len();
    assert!(
        len >= target,
        "updates.jsonl ({len} B) reached the target ({target} B)"
    );

    let replayed = load_updates_for_replay_at(info.id.0.as_ref(), root.path())
        .expect("load_updates_for_replay_at")
        .unwrap_or_default();
    assert!(!replayed.is_empty(), "the emitted updates parse back");
}
