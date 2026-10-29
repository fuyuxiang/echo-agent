//! Include the exact production helpers so recovery/cancellation regressions
//! run with desktop tests without the Runtime workspace's unrelated test graph.
pub use echo_agent_runtime::sampling::ConversationItem;
use echo_agent_runtime::session::memory::pending;

#[test]
fn long_turn_extraction_is_bounded_without_dropping_the_user_anchor_or_latest_outcome() {
    use echo_agent_runtime::session::helpers::memory_flush_window::{
        select_flush_window, MAX_FLUSH_BYTES, MAX_FLUSH_MESSAGES,
    };
    let mut history = vec![ConversationItem::user("original user request")];
    for _ in 0..190 {
        history.push(ConversationItem::assistant("文".repeat(100_000)));
    }
    history.push(ConversationItem::assistant("latest outcome"));
    let window = select_flush_window(history, 80);
    assert!(window.len() <= MAX_FLUSH_MESSAGES);
    assert!(
        window
            .iter()
            .map(|item| serde_json::to_vec(item).unwrap().len())
            .sum::<usize>()
            <= MAX_FLUSH_BYTES
    );
    assert_eq!(window[0].text_content(), "original user request");
    assert_eq!(window.last().unwrap().text_content(), "latest outcome");
}
#[path = "../../vendor/echo-agent-build/crates/codegen/echo-agent-runtime/src/session/memory/background.rs"]
mod background;

#[tokio::test]
async fn checkpointed_turns_survive_restart_and_failed_jobs_remain_retryable() {
    let root = tempfile::tempdir().unwrap();
    let storage = echo_agent_memory::MemoryStorage::with_paths(
        root.path().join("global"),
        root.path().join("workspace"),
    );
    for count in [1, 2] {
        let mut job: pending::PendingMemory = serde_json::from_value(serde_json::json!({
            "source_id": "one-source", "history": [], "conversation_len": count, "queued_at": 1
        }))
        .unwrap();
        pending::checkpoint(&storage, &job).unwrap();
        job.queued_at = 2;
        pending::checkpoint(&storage, &job).unwrap();
    }
    let count = || {
        std::fs::read_dir(storage.workspace_dir().join("pending"))
            .unwrap()
            .count()
    };
    assert_eq!(
        count(),
        2,
        "new turns must not overwrite earlier unprocessed checkpoints"
    );
    let edited = pending::PendingMemory {
        source_id: "one-source".into(),
        history: vec![ConversationItem::user("edited at the same history length")],
        conversation_len: 2,
        queued_at: 3,
    };
    pending::checkpoint(&storage, &edited).unwrap();
    assert_eq!(
        count(),
        3,
        "history length alone cannot identify edited or compacted content"
    );
    let seen = std::sync::Mutex::new(Vec::new());
    pending::recover_pending_jobs(
        &storage,
        || true,
        |job| {
            seen.lock().unwrap().push(job.conversation_len);
            async move { job.conversation_len == 2 }
        },
    )
    .await;
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert_eq!(count(), 1);
    pending::recover_pending_jobs(
        &storage,
        || false,
        |_| async { panic!("disabled recovery must not send anything") },
    )
    .await;
    assert_eq!(count(), 1);
    pending::recover_pending_jobs(&storage, || true, |_| async { true }).await;
    assert_eq!(count(), 0);
}

#[tokio::test]
async fn corrupt_checkpoints_do_not_starve_valid_jobs_and_remain_available_for_repair() {
    let root = tempfile::tempdir().unwrap();
    let storage = echo_agent_memory::MemoryStorage::with_paths(
        root.path().join("global"),
        root.path().join("workspace"),
    );
    let job = pending::PendingMemory {
        source_id: "valid-source".into(),
        history: vec![ConversationItem::user("valid request")],
        conversation_len: 1,
        queued_at: 1,
    };
    pending::checkpoint(&storage, &job).unwrap();
    let directory = storage.workspace_dir().join("pending");
    for n in 0..40 {
        std::fs::write(directory.join(format!("corrupt-{n}.json")), b"invalid").unwrap();
    }
    let seen = std::sync::atomic::AtomicUsize::new(0);
    pending::recover_pending_jobs(
        &storage,
        || true,
        |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            async { true }
        },
    )
    .await;
    assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        std::fs::read_dir(directory.join("quarantine"))
            .unwrap()
            .count(),
        40
    );
    assert_eq!(std::fs::read_dir(directory).unwrap().count(), 1);
}

#[test]
fn corrupt_current_checkpoint_is_preserved_and_replaced_without_blocking_exit_retry() {
    let root = tempfile::tempdir().unwrap();
    let storage = echo_agent_memory::MemoryStorage::with_paths(
        root.path().join("global"),
        root.path().join("workspace"),
    );
    let job = pending::PendingMemory {
        source_id: "current-source".into(),
        history: vec![ConversationItem::user("A useful current request")],
        conversation_len: 1,
        queued_at: 1,
    };
    pending::checkpoint(&storage, &job).unwrap();
    let path = storage
        .workspace_dir()
        .join("pending")
        .join(format!("{}.json", job.identity().unwrap()));
    std::fs::write(&path, b"invalid original bytes").unwrap();
    pending::checkpoint(&storage, &job).unwrap();
    pending::checkpoint(&storage, &job).unwrap();
    let restored: pending::PendingMemory =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(restored.identity().unwrap(), job.identity().unwrap());
    let archived: Vec<_> = std::fs::read_dir(storage.workspace_dir().join("pending/quarantine"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(
        archived.len(),
        1,
        "exit retries must not create duplicate archives"
    );
    assert_eq!(
        std::fs::read(&archived[0]).unwrap(),
        b"invalid original bytes"
    );
}

#[test]
fn a_mismatched_valid_job_is_preserved_before_replacing_its_checkpoint() {
    let root = tempfile::tempdir().unwrap();
    let storage = echo_agent_memory::MemoryStorage::with_paths(
        root.path().join("global"),
        root.path().join("workspace"),
    );
    let job = pending::PendingMemory {
        source_id: "current".into(),
        history: vec![],
        conversation_len: 1,
        queued_at: 1,
    };
    pending::checkpoint(&storage, &job).unwrap();
    let path = storage
        .workspace_dir()
        .join("pending")
        .join(format!("{}.json", job.identity().unwrap()));
    let other = pending::PendingMemory {
        source_id: "other".into(),
        history: vec![],
        conversation_len: 9,
        queued_at: 2,
    };
    std::fs::write(&path, serde_json::to_vec(&other).unwrap()).unwrap();
    pending::checkpoint(&storage, &job).unwrap();
    let archived = std::fs::read_dir(storage.workspace_dir().join("pending/quarantine"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let preserved: pending::PendingMemory =
        serde_json::from_slice(&std::fs::read(archived).unwrap()).unwrap();
    assert_eq!(preserved.source_id, "other");
}

#[test]
fn isolation_failure_remains_a_save_error_and_keeps_the_original() {
    let root = tempfile::tempdir().unwrap();
    let storage = echo_agent_memory::MemoryStorage::with_paths(
        root.path().join("global"),
        root.path().join("workspace"),
    );
    let job = pending::PendingMemory {
        source_id: "current".into(),
        history: vec![],
        conversation_len: 1,
        queued_at: 1,
    };
    pending::checkpoint(&storage, &job).unwrap();
    let path = storage
        .workspace_dir()
        .join("pending")
        .join(format!("{}.json", job.identity().unwrap()));
    std::fs::write(&path, b"damaged").unwrap();
    std::fs::write(
        storage.workspace_dir().join("pending/quarantine"),
        b"blocked directory",
    )
    .unwrap();
    assert!(pending::checkpoint(&storage, &job).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"damaged");
}

#[test]
fn oversized_checkpoint_is_archived_without_reading_it_as_a_job() {
    let root = tempfile::tempdir().unwrap();
    let storage = echo_agent_memory::MemoryStorage::with_paths(
        root.path().join("global"),
        root.path().join("workspace"),
    );
    let job = pending::PendingMemory {
        source_id: "current".into(),
        history: vec![],
        conversation_len: 1,
        queued_at: 1,
    };
    pending::checkpoint(&storage, &job).unwrap();
    let path = storage
        .workspace_dir()
        .join("pending")
        .join(format!("{}.json", job.identity().unwrap()));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();
    pending::checkpoint(&storage, &job).unwrap();
    let archived = std::fs::read_dir(storage.workspace_dir().join("pending/quarantine"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::metadata(archived).unwrap().len(),
        8 * 1024 * 1024 + 1
    );
    assert!(std::fs::metadata(path).unwrap().len() < 4096);
}
