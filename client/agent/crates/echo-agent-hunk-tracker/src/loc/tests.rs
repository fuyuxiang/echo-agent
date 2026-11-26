use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use tokio::sync::mpsc;

use crate::events::HunkEvent;
use crate::types::{Hunk, HunkId, HunkLineInfo, HunkSource};

use super::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn sample_agent_hunk() -> Hunk {
    Hunk {
        id: HunkId::from_string("test-hunk-001".into()),
        path: PathBuf::from("/tmp/foo.rs"),
        line_info: HunkLineInfo {
            old_start: 10,
            old_count: 3,
            new_start: 10,
            new_count: 5,
        },
        source: HunkSource::AgentEdit { prompt_index: 2 },
        old_text: Some("old\nlines\nhere".into()),
        new_text: "new\nlines\nhere\nplus\nmore".into(),
        patch: None,
        created_at: Utc::now(),
        selected: false,
    }
}

fn sample_external_hunk() -> Hunk {
    Hunk {
        id: HunkId::from_string("test-hunk-002".into()),
        path: PathBuf::from("/tmp/bar.rs"),
        line_info: HunkLineInfo {
            old_start: 1,
            old_count: 0,
            new_start: 1,
            new_count: 4,
        },
        source: HunkSource::External,
        old_text: None,
        new_text: "line1\nline2\nline3\nline4".into(),
        patch: None,
        created_at: Utc::now(),
        selected: false,
    }
}

fn sample_deletion_hunk() -> Hunk {
    Hunk {
        id: HunkId::from_string("test-hunk-003".into()),
        path: PathBuf::from("/tmp/del.rs"),
        line_info: HunkLineInfo {
            old_start: 5,
            old_count: 3,
            new_start: 0,
            new_count: 0,
        },
        source: HunkSource::AgentEdit { prompt_index: 1 },
        old_text: Some("deleted\nlines\nhere".into()),
        new_text: String::new(),
        patch: None,
        created_at: Utc::now(),
        selected: false,
    }
}

/// In-memory writer for testing.
struct VecWriter {
    records: Vec<HunkRecord>,
    flush_count: usize,
}

impl VecWriter {
    fn new() -> Self {
        Self {
            records: Vec::new(),
            flush_count: 0,
        }
    }
}

impl HunkRecordWriter for VecWriter {
    async fn write(&mut self, record: &HunkRecord) -> std::io::Result<()> {
        self.records.push(record.clone());
        Ok(())
    }

    async fn flush(&mut self) -> std::io::Result<()> {
        self.flush_count += 1;
        Ok(())
    }
}

/// Thread-safe wrapper around `VecWriter` for use with `run_loc_sink`
/// (which takes ownership of the writer).
struct SharedWriter(std::sync::Arc<std::sync::Mutex<VecWriter>>);

impl SharedWriter {
    fn new() -> (Self, std::sync::Arc<std::sync::Mutex<VecWriter>>) {
        let inner = std::sync::Arc::new(std::sync::Mutex::new(VecWriter::new()));
        (Self(inner.clone()), inner)
    }
}

impl HunkRecordWriter for SharedWriter {
    async fn write(&mut self, record: &HunkRecord) -> std::io::Result<()> {
        // Do the work inside the lock synchronously — don't hold MutexGuard across .await
        self.0.lock().unwrap().records.push(record.clone());
        Ok(())
    }
    async fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().flush_count += 1;
        Ok(())
    }
}

fn make_ctx() -> LocSinkContext {
    LocSinkContext {
        session_id: "sess-001".into(),
        agent_id: "agent-abc".into(),
        user_id: Some("user-xyz".into()),
        aggregate_tx: None,
    }
}

// ---------------------------------------------------------------------------
// Unit tests: HunkRecord::from_hunk
// ---------------------------------------------------------------------------

#[test]
fn from_hunk_agent_edit() {
    let hunk = sample_agent_hunk();
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-1",
        "agent-1",
        Some("user-1"),
        EventType::Added,
        &hunk.source,
    );

    assert_eq!(record.hunk_id, HunkId::from_string("test-hunk-001".into()));
    assert_eq!(record.file_path, PathBuf::from("/tmp/foo.rs"));
    assert_eq!(record.hunk_start, 10);
    assert_eq!(record.hunk_end, 14); // 10 + 5 - 1
    assert_eq!(record.lines_added, 5);
    assert_eq!(record.lines_removed, 3);
    assert_eq!(record.author_type, Some(AuthorType::Agent));
    assert_eq!(record.author_id, Some("agent-1".into()));
    assert_eq!(record.agent_id, "agent-1");
    assert_eq!(record.session_id, "sess-1");
    assert_eq!(record.prompt_index, Some(2));
    assert_eq!(record.source_type, Some(SourceType::AgentEdit));
    assert_eq!(record.event_type, EventType::Added);
}

#[test]
fn from_hunk_event_type_updated() {
    let hunk = sample_agent_hunk();
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-1",
        "agent-1",
        None,
        EventType::Updated,
        &hunk.source,
    );
    assert_eq!(record.event_type, EventType::Updated);
}

#[test]
fn from_hunk_external() {
    let hunk = sample_external_hunk();
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-1",
        "agent-1",
        Some("user-1"),
        EventType::Added,
        &hunk.source,
    );

    assert_eq!(record.author_type, Some(AuthorType::Human));
    assert_eq!(record.author_id, Some("user-1".into()));
    assert_eq!(record.prompt_index, None);
    assert_eq!(record.source_type, Some(SourceType::External));
    assert_eq!(record.hunk_start, 1);
    assert_eq!(record.hunk_end, 4); // 1 + 4 - 1
}

#[test]
fn from_hunk_external_no_user_id() {
    let hunk = sample_external_hunk();
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-1",
        "agent-1",
        None,
        EventType::Added,
        &hunk.source,
    );

    assert_eq!(record.author_type, Some(AuthorType::Human));
    assert_eq!(record.author_id, None);
}

#[test]
fn from_hunk_external_edit_on_agent_file() {
    let mut hunk = sample_external_hunk();
    hunk.source = HunkSource::ExternalEditOnAgentFile;
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-1",
        "agent-1",
        Some("user-1"),
        EventType::Added,
        &hunk.source,
    );

    assert_eq!(record.author_type, Some(AuthorType::Human));
    assert_eq!(
        record.source_type,
        Some(SourceType::ExternalEditOnAgentFile)
    );
}

#[test]
fn from_hunk_pure_deletion() {
    let hunk = sample_deletion_hunk();
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-1",
        "agent-1",
        None,
        EventType::Added,
        &hunk.source,
    );

    // Pure deletion: new_count == 0, so uses old_start/old_count
    assert_eq!(record.hunk_start, 5);
    assert_eq!(record.hunk_end, 7); // 5 + 3 - 1
    assert_eq!(record.lines_added, 0i64);
    assert_eq!(record.lines_removed, 3i64);
}

/// Verify that attribution_source overrides the hunk's preserved source.
#[test]
fn from_hunk_trigger_source_overrides_preserved_source() {
    let hunk = sample_agent_hunk(); // hunk.source = AgentEdit
    let trigger = HunkSource::ExternalEditOnAgentFile;
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-1",
        "agent-1",
        Some("user-1"),
        EventType::Updated,
        &trigger,
    );

    assert_eq!(record.author_type, Some(AuthorType::Human));
    assert_eq!(
        record.source_type,
        Some(SourceType::ExternalEditOnAgentFile)
    );
    assert_eq!(record.author_id, Some("user-1".into()));
    assert_eq!(record.prompt_index, None);
    assert_eq!(record.event_type, EventType::Updated);
}

// ---------------------------------------------------------------------------
// Sink tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sink_processes_added_and_content_changed() {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = make_ctx();
    let cancel = tokio_util::sync::CancellationToken::new();

    let hunk = sample_agent_hunk();
    let mut updated_hunk = sample_agent_hunk();
    updated_hunk.line_info.new_count = 8; // grew from 5 to 8 lines

    // Send a mix of events — only HunkAdded and HunkContentChanged should produce records
    tx.send(HunkEvent::FileAdded {
        path: PathBuf::from("/tmp/foo.rs"),
        is_agent_file: true,
    })
    .unwrap();
    tx.send(HunkEvent::HunkAdded {
        path: PathBuf::from("/tmp/foo.rs"),
        hunk: Arc::new(hunk),
    })
    .unwrap();
    tx.send(HunkEvent::HunkContentChanged {
        path: PathBuf::from("/tmp/foo.rs"),
        hunk: Arc::new(updated_hunk),
        trigger_source: HunkSource::AgentEdit { prompt_index: 2 },
        prev_lines_added: 5,   // original hunk had 5 lines added
        prev_lines_removed: 3, // original hunk had 3 lines removed
    })
    .unwrap();
    tx.send(HunkEvent::HunkMoved {
        path: PathBuf::from("/tmp/foo.rs"),
        hunk_id: HunkId::from_string("test-hunk-001".into()),
        new_line_info: HunkLineInfo {
            old_start: 10,
            old_count: 3,
            new_start: 12,
            new_count: 5,
        },
    })
    .unwrap();
    tx.send(HunkEvent::HunkRemoved {
        path: PathBuf::from("/tmp/foo.rs"),
        hunk_id: HunkId::from_string("test-hunk-001".into()),
        reason: crate::events::HunkRemovalReason::Superseded,
    })
    .unwrap();
    tx.send(HunkEvent::FileRemoved {
        path: PathBuf::from("/tmp/foo.rs"),
    })
    .unwrap();
    tx.send(HunkEvent::BaselineUpdated {
        path: PathBuf::from("/tmp/foo.rs"),
    })
    .unwrap();

    // Drop sender to close the channel
    drop(tx);

    let (shared_writer, shared) = SharedWriter::new();
    run_loc_sink(rx, shared_writer, ctx, cancel).await;

    let w = shared.lock().unwrap();
    assert_eq!(
        w.records.len(),
        3,
        "HunkAdded + HunkContentChanged + HunkRemoved should produce 3 records"
    );

    // First record: added (full counts)
    assert_eq!(
        w.records[0].hunk_id,
        HunkId::from_string("test-hunk-001".into())
    );
    assert_eq!(w.records[0].event_type, EventType::Added);
    assert_eq!(w.records[0].lines_added, 5);
    assert_eq!(w.records[0].lines_removed, 3);

    // Second record: updated (delta: 8-5=3 added, 3-3=0 removed)
    assert_eq!(w.records[1].event_type, EventType::Updated);
    assert_eq!(w.records[1].lines_added, 3i64);
    assert_eq!(w.records[1].lines_removed, 0i64);

    // Third record: removed (negates accumulated: -(5+3)=-8, -(3+0)=-3)
    assert_eq!(w.records[2].event_type, EventType::Removed);
    assert_eq!(w.records[2].lines_added, -8i64);
    assert_eq!(w.records[2].lines_removed, -3i64);

    // SUM should be zero
    let total: i64 = w.records.iter().map(|r| r.lines_added).sum();
    assert_eq!(total, 0);
}

/// When a hunk is removed, the sink must emit a negating record so that
/// SUM-based totals zero out the hunk's contribution.
#[tokio::test]
async fn sink_removed_hunk_zeroes_out_accumulated_total() {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = make_ctx();
    let cancel = tokio_util::sync::CancellationToken::new();

    let hunk = sample_agent_hunk(); // lines_added=5, lines_removed=3
    let hunk_id = hunk.id.clone();
    let path = hunk.path.clone();

    // Add, then remove
    tx.send(HunkEvent::HunkAdded {
        path: path.clone(),
        hunk: Arc::new(hunk),
    })
    .unwrap();
    tx.send(HunkEvent::HunkRemoved {
        path: path.clone(),
        hunk_id: hunk_id.clone(),
        reason: crate::events::HunkRemovalReason::Rejected,
    })
    .unwrap();
    drop(tx);

    let (shared_writer, shared) = SharedWriter::new();
    run_loc_sink(rx, shared_writer, ctx, cancel).await;

    let w = shared.lock().unwrap();
    assert_eq!(w.records.len(), 2, "Should have added + removed records");

    // First: added
    assert_eq!(w.records[0].event_type, EventType::Added);
    assert_eq!(w.records[0].lines_added, 5);
    assert_eq!(w.records[0].lines_removed, 3);

    // Second: removed (negated)
    assert_eq!(w.records[1].event_type, EventType::Removed);
    assert_eq!(w.records[1].lines_added, -5);
    assert_eq!(w.records[1].lines_removed, -3);

    // SUM should be zero
    let total_added: i64 = w.records.iter().map(|r| r.lines_added).sum();
    let total_removed: i64 = w.records.iter().map(|r| r.lines_removed).sum();
    assert_eq!(total_added, 0, "Removed hunk should zero out lines_added");
    assert_eq!(
        total_removed, 0,
        "Removed hunk should zero out lines_removed"
    );
}

/// Full scenario: agent adds, human expands, then hunk is removed.
/// The negating record must cancel the entire accumulated total.
#[tokio::test]
async fn sink_removed_hunk_after_updates_zeroes_correctly() {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = make_ctx();
    let cancel = tokio_util::sync::CancellationToken::new();

    let hunk = sample_agent_hunk(); // lines_added=5, lines_removed=3
    let hunk_id = hunk.id.clone();
    let path = hunk.path.clone();

    let mut updated = sample_agent_hunk();
    updated.line_info.new_count = 8; // grew from 5 → 8

    // Add → update → remove
    tx.send(HunkEvent::HunkAdded {
        path: path.clone(),
        hunk: Arc::new(hunk),
    })
    .unwrap();
    tx.send(HunkEvent::HunkContentChanged {
        path: path.clone(),
        hunk: Arc::new(updated),
        trigger_source: HunkSource::ExternalEditOnAgentFile,
        prev_lines_added: 5,
        prev_lines_removed: 3,
    })
    .unwrap();
    tx.send(HunkEvent::HunkRemoved {
        path: path.clone(),
        hunk_id: hunk_id.clone(),
        reason: crate::events::HunkRemovalReason::Superseded,
    })
    .unwrap();
    drop(tx);

    let (shared_writer, shared) = SharedWriter::new();
    run_loc_sink(rx, shared_writer, ctx, cancel).await;

    let w = shared.lock().unwrap();
    assert_eq!(w.records.len(), 3, "added + updated + removed");

    // SUM should be zero: the hunk was fully removed
    let total_added: i64 = w.records.iter().map(|r| r.lines_added).sum();
    let total_removed: i64 = w.records.iter().map(|r| r.lines_removed).sum();
    assert_eq!(total_added, 0);
    assert_eq!(total_removed, 0);
}

/// Accepted hunks keep their LOC contribution — no negating record is written.
#[tokio::test]
async fn sink_accepted_hunk_preserves_loc() {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = make_ctx();
    let cancel = tokio_util::sync::CancellationToken::new();

    let hunk = sample_agent_hunk(); // lines_added=5, lines_removed=3
    let hunk_id = hunk.id.clone();
    let path = hunk.path.clone();

    tx.send(HunkEvent::HunkAdded {
        path: path.clone(),
        hunk: Arc::new(hunk),
    })
    .unwrap();
    tx.send(HunkEvent::HunkRemoved {
        path: path.clone(),
        hunk_id: hunk_id.clone(),
        reason: crate::events::HunkRemovalReason::Accepted,
    })
    .unwrap();
    drop(tx);

    let (shared_writer, shared) = SharedWriter::new();
    run_loc_sink(rx, shared_writer, ctx, cancel).await;

    let w = shared.lock().unwrap();
    // Only the Added record — no Removed record for accepted hunks
    assert_eq!(
        w.records.len(),
        1,
        "Accepted hunk should NOT produce a Removed record"
    );
    assert_eq!(w.records[0].event_type, EventType::Added);

    // LOC is preserved
    let total_added: i64 = w.records.iter().map(|r| r.lines_added).sum();
    assert_eq!(total_added, 5, "Accepted hunk's LOC should be preserved");
}

/// When a hunk *shrinks* (e.g., human deletes 3 of 10 agent lines), the
/// delta must be negative so SUM-based LOC totals stay accurate.
#[tokio::test]
async fn sink_shrinking_hunk_produces_negative_delta() {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = make_ctx();
    let cancel = tokio_util::sync::CancellationToken::new();

    // Agent adds 10 lines
    let mut hunk = sample_agent_hunk();
    hunk.line_info.new_count = 10;
    hunk.line_info.old_count = 0;

    // Human deletes 3 → hunk shrinks to 7
    let mut shrunk = sample_agent_hunk();
    shrunk.line_info.new_count = 7;
    shrunk.line_info.old_count = 0;

    tx.send(HunkEvent::HunkAdded {
        path: hunk.path.clone(),
        hunk: Arc::new(hunk),
    })
    .unwrap();
    tx.send(HunkEvent::HunkContentChanged {
        path: shrunk.path.clone(),
        hunk: Arc::new(shrunk),
        trigger_source: HunkSource::ExternalEditOnAgentFile,
        prev_lines_added: 10,
        prev_lines_removed: 0,
    })
    .unwrap();
    drop(tx);

    let (shared_writer, shared) = SharedWriter::new();
    run_loc_sink(rx, shared_writer, ctx, cancel).await;

    let w = shared.lock().unwrap();
    assert_eq!(w.records.len(), 2);

    // First: agent added 10 lines
    assert_eq!(w.records[0].author_type, Some(AuthorType::Agent));
    assert_eq!(w.records[0].lines_added, 10i64);

    // Second: human shrunk the hunk by 3 → negative delta
    assert_eq!(w.records[1].author_type, Some(AuthorType::Human));
    assert_eq!(w.records[1].event_type, EventType::Updated);
    assert_eq!(w.records[1].lines_added, -3i64);
    assert_eq!(w.records[1].lines_removed, 0i64);

    // SUM(lines_added) by author: agent=10, human=-3, net=7 ✅
    let agent_total: i64 = w
        .records
        .iter()
        .filter(|r| r.author_type == Some(AuthorType::Agent))
        .map(|r| r.lines_added)
        .sum();
    let human_total: i64 = w
        .records
        .iter()
        .filter(|r| r.author_type == Some(AuthorType::Human))
        .map(|r| r.lines_added)
        .sum();
    assert_eq!(agent_total, 10);
    assert_eq!(human_total, -3);
    assert_eq!(agent_total + human_total, 7); // net lines in file
}

#[tokio::test]
async fn sink_drains_on_cancellation() {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = make_ctx();
    let cancel = tokio_util::sync::CancellationToken::new();

    let hunk1 = sample_agent_hunk();
    let hunk2 = sample_external_hunk();

    // Send events before cancellation
    tx.send(HunkEvent::HunkAdded {
        path: hunk1.path.clone(),
        hunk: Arc::new(hunk1),
    })
    .unwrap();
    tx.send(HunkEvent::HunkAdded {
        path: hunk2.path.clone(),
        hunk: Arc::new(hunk2),
    })
    .unwrap();

    // Cancel immediately
    cancel.cancel();

    let (shared_writer, shared) = SharedWriter::new();
    run_loc_sink(rx, shared_writer, ctx, cancel).await;

    let w = shared.lock().unwrap();
    assert_eq!(
        w.records.len(),
        2,
        "Both events should be drained on cancellation"
    );
    assert!(w.flush_count > 0, "Writer should be flushed on shutdown");
}

// ---------------------------------------------------------------------------
// JSONL round-trip test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn jsonl_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hunk_records.jsonl");
    let mut writer = JsonlHunkRecordWriter::new(path.clone());

    let hunk = sample_agent_hunk();
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-rt",
        "agent-rt",
        Some("user-rt"),
        EventType::Added,
        &hunk.source,
    );

    writer.write(&record).await.unwrap();
    writer.flush().await.unwrap();

    // Read back and deserialize
    let contents = tokio::fs::read_to_string(&path).await.unwrap();
    let lines: Vec<&str> = contents.trim().lines().collect();
    assert_eq!(lines.len(), 1);

    let deserialized: HunkRecord = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(deserialized, record);
}

#[tokio::test]
async fn jsonl_writer_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested").join("deep").join("records.jsonl");
    let mut writer = JsonlHunkRecordWriter::new(path.clone());

    let hunk = sample_agent_hunk();
    let record = HunkRecord::from_hunk(
        &hunk,
        "sess-1",
        "agent-1",
        None,
        EventType::Added,
        &hunk.source,
    );

    writer.write(&record).await.unwrap();
    assert!(path.exists());
}

#[tokio::test]
async fn jsonl_writer_appends() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records.jsonl");
    let mut writer = JsonlHunkRecordWriter::new(path.clone());

    let hunk1 = sample_agent_hunk();
    let hunk2 = sample_external_hunk();
    let r1 = HunkRecord::from_hunk(&hunk1, "s", "a", None, EventType::Added, &hunk1.source);
    let r2 = HunkRecord::from_hunk(&hunk2, "s", "a", None, EventType::Added, &hunk2.source);

    writer.write(&r1).await.unwrap();
    writer.write(&r2).await.unwrap();
    writer.flush().await.unwrap();

    let contents = tokio::fs::read_to_string(&path).await.unwrap();
    let lines: Vec<&str> = contents.trim().lines().collect();
    assert_eq!(lines.len(), 2);
}

// ---------------------------------------------------------------------------
// Deserialization validation
// ---------------------------------------------------------------------------

/// Invalid enum values must be rejected during deserialization.
/// This validates that the serde enum gate works — a typo like "foo"
/// in the JSONL can't silently sneak past.
#[test]
fn deserialize_rejects_invalid_author_type() {
    let hunk = sample_agent_hunk();
    let record = HunkRecord::from_hunk(&hunk, "s", "a", None, EventType::Added, &hunk.source);
    let mut json = serde_json::to_string(&record).unwrap();

    // Replace valid "agent" with invalid "foo"
    json = json.replacen("\"agent\"", "\"foo\"", 1);
    let result = serde_json::from_str::<HunkRecord>(&json);
    assert!(result.is_err(), "Should reject invalid author_type");
}

#[test]
fn deserialize_rejects_invalid_event_type() {
    let hunk = sample_agent_hunk();
    let record = HunkRecord::from_hunk(&hunk, "s", "a", None, EventType::Added, &hunk.source);
    let mut json = serde_json::to_string(&record).unwrap();

    json = json.replacen("\"added\"", "\"foo\"", 1);
    let result = serde_json::from_str::<HunkRecord>(&json);
    assert!(result.is_err(), "Should reject invalid event_type");
}

#[test]
fn deserialize_rejects_invalid_source_type() {
    let hunk = sample_agent_hunk();
    let record = HunkRecord::from_hunk(&hunk, "s", "a", None, EventType::Added, &hunk.source);
    let mut json = serde_json::to_string(&record).unwrap();

    json = json.replacen("\"agentEdit\"", "\"foo\"", 1);
    let result = serde_json::from_str::<HunkRecord>(&json);
    assert!(result.is_err(), "Should reject invalid source_type");
}

// ---------------------------------------------------------------------------
// Writer failure resilience
// ---------------------------------------------------------------------------

/// The sink must continue processing events even when the writer fails.
/// This validates the "log warning and drop the record" error policy.
#[tokio::test]
async fn sink_continues_after_writer_failure() {
    let (tx, rx) = mpsc::unbounded_channel();
    let ctx = make_ctx();
    let cancel = tokio_util::sync::CancellationToken::new();

    let hunk1 = sample_agent_hunk();
    let hunk2 = sample_external_hunk();

    tx.send(HunkEvent::HunkAdded {
        path: hunk1.path.clone(),
        hunk: Arc::new(hunk1),
    })
    .unwrap();
    tx.send(HunkEvent::HunkAdded {
        path: hunk2.path.clone(),
        hunk: Arc::new(hunk2),
    })
    .unwrap();
    drop(tx);

    /// Writer that always fails on write but tracks flush calls.
    struct FailingWriter(std::sync::Arc<std::sync::Mutex<bool>>);

    impl HunkRecordWriter for FailingWriter {
        async fn write(&mut self, _record: &HunkRecord) -> std::io::Result<()> {
            Err(std::io::Error::other("disk full"))
        }
        async fn flush(&mut self) -> std::io::Result<()> {
            *self.0.lock().unwrap() = true;
            Ok(())
        }
    }

    let flush_called = std::sync::Arc::new(std::sync::Mutex::new(false));
    let writer = FailingWriter(flush_called.clone());

    // This must not panic — the sink should log warnings and continue.
    run_loc_sink(rx, writer, ctx, cancel).await;

    // Flush must still be called on shutdown (sink didn't abort early).
    assert!(
        *flush_called.lock().unwrap(),
        "Sink should flush on shutdown even after write failures"
    );
}
