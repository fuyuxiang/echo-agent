//! Adapter-driven session synthesis for benches: appends realistic turns through
//! the real `JsonlStorageAdapter` until `updates.jsonl` reaches a byte target,
//! so fork/copy benchmarks measure production-shaped data.

use std::path::Path;

use agent_client_protocol::{self as acp};

use crate::session::info::Info;
use crate::session::storage::{JsonlStorageAdapter, SessionUpdate, StorageAdapter};

const AGENT_CHUNKS_PER_TURN: usize = 8;
/// Stands in for a large tool result, the dominant byte source in real
/// sessions. Emitted as an agent message chunk so the byte and line shape match
/// production rather than the `ToolCall` kind.
const BULKY_CHUNK_BYTES: usize = 4096;

fn turn_updates(info: &Info, turn: usize) -> Vec<SessionUpdate> {
    let text =
        |s: String| acp::ContentChunk::new(acp::ContentBlock::Text(acp::TextContent::new(s)));
    let notify =
        |u| SessionUpdate::Acp(Box::new(acp::SessionNotification::new(info.id.clone(), u)));
    let mut updates = vec![notify(acp::SessionUpdate::UserMessageChunk(text(format!(
        "prompt {turn}: check the build and summarize failures"
    ))))];
    for i in 0..AGENT_CHUNKS_PER_TURN {
        updates.push(notify(acp::SessionUpdate::AgentMessageChunk(text(format!(
            "agent chunk {turn}/{i}: analyzing module {i} for regressions and drafting a fix plan"
        )))));
    }
    updates.push(notify(acp::SessionUpdate::AgentMessageChunk(text(
        format!("bulky chunk {turn}: {}", "x".repeat(BULKY_CHUNK_BYTES)),
    ))));
    updates
}

/// Build a session dir under `root` whose `updates.jsonl` reaches *at least*
/// `target_bytes`, appending realistic mixed updates through the real adapter.
/// The file overshoots to the next 32-turn stat boundary, so the result is a
/// floor, not an exact size.
///
/// Async callers await this directly; synchronous callers (Criterion benches,
/// plain `#[test]`s) use [`make_session_with_size_blocking`].
pub async fn make_session_with_size(root: &Path, target_bytes: u64) -> Info {
    let adapter = JsonlStorageAdapter::with_root(root.to_path_buf());
    let info = Info {
        id: acp::SessionId::new("fork-bench-src"),
        cwd: "/bench/workspace".to_string(),
    };
    adapter
        .init_session(&info, acp::ModelId::new("bench-model"))
        .await
        .expect("init session");
    let updates_path = adapter.updates_file_path(&info).expect("updates path");
    let mut turn = 0usize;
    loop {
        for update in turn_updates(&info, turn) {
            adapter.append_update(&info, &update).await.expect("append");
        }
        turn += 1;
        // Stat every 32 turns; sizes only grow. A persistent stat failure
        // panics here rather than spinning the append loop forever.
        if turn.is_multiple_of(32)
            && std::fs::metadata(&updates_path)
                .expect("stat updates.jsonl")
                .len()
                >= target_bytes
        {
            break;
        }
    }
    info
}

/// Synchronous wrapper over [`make_session_with_size`] for callers outside an
/// async context (Criterion benches, plain `#[test]`s). Drives the async core
/// on a private current-thread runtime.
pub fn make_session_with_size_blocking(root: &Path, target_bytes: u64) -> Info {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench runtime")
        .block_on(make_session_with_size(root, target_bytes))
}
