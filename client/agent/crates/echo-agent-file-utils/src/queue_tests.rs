use super::*;
use crate::UploadMethod;

/// Mock credential resolver for tests.
struct MockResolver;

impl TraceExportSource for MockResolver {
    fn resolve(&self) -> TraceExportConfig {
        TraceExportConfig {
            bucket_url: Some("gs://test-bucket".to_string()),
            service_account_key: None,
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
            upload_method: UploadMethod::Direct {
                service_account_key: None,
            },
        }
    }
}

/// Test wrapper for [`upload_with_retries`] supplying fresh stats, a
/// never-draining flag, and no concurrency permit (these tests don't
/// exercise the worker semaphore).
async fn run_upload_with_retries(
    item: &mut UploadQueueItem,
    resolver: &Arc<dyn TraceExportSource>,
    policy: &UploadRetryPolicy,
) -> anyhow::Result<(String, BlobCompression, u64)> {
    upload_with_retries(
        item,
        resolver,
        policy,
        100,
        &Arc::new(UploadQueueStats::new()),
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await
}

#[tokio::test]
async fn transition_notify_wakes_wired_listener() {
    let stats = Arc::new(UploadQueueStats::new());
    let notify = Arc::new(Notify::new());
    stats.set_transition_notify(notify.clone());

    let waiter = {
        let n = notify.clone();
        tokio::spawn(async move { n.notified().await })
    };
    tokio::task::yield_now().await;

    stats.notify_transition();

    tokio::time::timeout(Duration::from_secs(2), waiter)
        .await
        .expect("listener must wake on a queue transition")
        .expect("waiter task should not panic");
}

/// A shutdown that interrupts the breaker cooldown must not leave
/// `circuit_breaker_active` stuck `true`.
#[tokio::test]
async fn circuit_breaker_cooldown_clears_active_flag_on_shutdown() {
    let stats = Arc::new(UploadQueueStats::new());
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    // Deliver shutdown up front so it wins the cooldown `select!` immediately.
    shutdown_tx.send(()).unwrap();
    tokio::pin!(shutdown_rx);

    let interrupted = circuit_breaker_cooldown(&stats, shutdown_rx.as_mut()).await;

    assert!(
        interrupted,
        "a delivered shutdown must interrupt the cooldown"
    );
    assert!(
        !stats.circuit_breaker_active.load(Ordering::Relaxed),
        "the live breaker gauge must be cleared when shutdown interrupts an active breaker"
    );
}

/// Unwired stats treat the transition ping as a no-op; set is once-only.
#[test]
fn transition_notify_without_listener_is_noop() {
    let stats = UploadQueueStats::new();
    stats.notify_transition();
    let first = Arc::new(Notify::new());
    let second = Arc::new(Notify::new());
    stats.set_transition_notify(first.clone());
    stats.set_transition_notify(second);
    assert!(
        stats.transition_notify.get().is_some(),
        "a notifier must be installed after the first set"
    );
}

/// The per-turn flush contract: empty queue returns immediately, a missed
/// deadline reports (never aborts) the remaining count, and a settle wakes
/// the waiter — all without touching the worker.
#[tokio::test]
async fn wait_idle_reports_pending_and_wakes_on_settle() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = UploadQueue {
        tx,
        queue_dir,
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    assert_eq!(
        queue.wait_idle(Duration::from_millis(10)).await,
        0,
        "empty queue is already idle"
    );

    stats.pending.fetch_add(2, Ordering::Relaxed);
    assert_eq!(
        queue.wait_idle(Duration::from_millis(50)).await,
        2,
        "deadline reports the remaining count"
    );

    let settle_stats = stats.clone();
    let settle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        settle_stats.pending.store(0, Ordering::Relaxed);
        settle_stats.notify_transition();
    });
    assert_eq!(
        queue.wait_idle(Duration::from_secs(5)).await,
        0,
        "a settle wakes the waiter before the deadline"
    );
    settle.await.unwrap();
}

/// A blocking enqueue spills as a temp + sidecar pair before any await,
/// so an item outliving its waiter (cancelled confirmation, process exit)
/// is exactly what `run_startup_recovery` re-enqueues next run.
#[tokio::test]
async fn blocking_enqueue_spills_recoverable_sidecar_pair() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(1);
    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats,
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    // No worker: the accepted item parks on confirmation until the caller
    // gives up, exactly the state a process exit would strand.
    let content = b"session-state-bytes";
    let _ = tokio::time::timeout(
        Duration::from_millis(200),
        queue.enqueue_blocking(
            content,
            "sess-1234/turn_7/tool_state.json",
            "application/gzip",
            "session_state",
            "sess-1234",
            7,
        ),
    )
    .await;

    let sidecars: Vec<_> = std::fs::read_dir(&queue_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(SIDECAR_SUFFIX))
        .collect();
    assert_eq!(sidecars.len(), 1, "one sidecar spilled");
    let sidecar: QueueItemSidecar =
        serde_json::from_slice(&std::fs::read(&sidecars[0]).unwrap()).unwrap();
    assert_eq!(sidecar.gcs_path, "sess-1234/turn_7/tool_state.json");
    assert_eq!(
        sidecar.sha256,
        crate::sha256_hex(content),
        "recovery's corruption guard must accept the pair"
    );
    let temp_file = temp_path_for_sidecar(&sidecars[0]).unwrap();
    assert!(temp_file.exists(), "the pair's temp file is in place");
}

/// A blocking enqueue rejected by the channel (full here; closed behaves
/// the same) must roll back `pending` before any await, so a cancelled or
/// failed hand-off can never leak the counter and poison `wait_idle` into
/// full-budget stalls for the rest of the session.
#[tokio::test]
async fn rejected_blocking_enqueue_does_not_leak_pending() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let stats = Arc::new(UploadQueueStats::new());
    // Capacity-1 channel with no worker: the second send is rejected Full,
    // exercising the rollback + inline-fallback branch deterministically.
    let (tx, _rx) = mpsc::channel(1);
    let queue = UploadQueue {
        tx,
        queue_dir,
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    let filler = tokio::time::timeout(
        Duration::from_millis(200),
        queue.enqueue_blocking(
            b"filler",
            "s/turn_0/a.json",
            "application/json",
            "a",
            "s",
            0,
        ),
    )
    .await;
    assert!(
        filler.is_err(),
        "no worker: the accepted item never settles"
    );
    assert_eq!(
        stats.pending.load(Ordering::Relaxed),
        1,
        "the accepted item is the only pending one"
    );

    // Full channel: rejected before any await, diverted inline; `pending`
    // must be back to the accepted item only.
    let overflow = tokio::time::timeout(
        Duration::from_millis(200),
        queue.enqueue_blocking(
            b"overflow",
            "s/turn_0/b.json",
            "application/json",
            "b",
            "s",
            0,
        ),
    )
    .await;
    drop(overflow);
    assert_eq!(
        stats.pending.load(Ordering::Relaxed),
        1,
        "a rejected hand-off must not leak pending"
    );
    assert_eq!(
        stats.enqueued.load(Ordering::Relaxed),
        1,
        "a diverted item must not count as enqueued"
    );
    assert_eq!(
        stats.enqueue_fallbacks.load(Ordering::Relaxed),
        1,
        "the overflow item diverted to the inline fallback"
    );
}

#[test]
fn retry_policy_backoff_increases_exponentially() {
    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(60),
        multiplier: 2.0,
        ..Default::default()
    };

    assert_eq!(policy.backoff_delay(0), Duration::from_secs(1));
    assert_eq!(policy.backoff_delay(1), Duration::from_secs(2));
    assert_eq!(policy.backoff_delay(2), Duration::from_secs(4));
    assert_eq!(policy.backoff_delay(3), Duration::from_secs(8));
}

#[test]
fn retry_policy_backoff_capped_at_max() {
    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_secs(1),
        max_delay: Duration::from_secs(10),
        multiplier: 2.0,
        ..Default::default()
    };

    // 2^5 = 32, but capped at 10
    assert_eq!(policy.backoff_delay(5), Duration::from_secs(10));
    assert_eq!(policy.backoff_delay(10), Duration::from_secs(10));
}

#[test]
fn auth_park_probe_override_rejects_zero_and_floors() {
    // 0 would re-probe every parked item on every wait slice — reject it
    // so the default interval stands.
    assert_eq!(auth_park_probe_override(0), None);
    assert_eq!(auth_park_probe_override(1), Some(Duration::from_secs(1)));
    assert_eq!(auth_park_probe_override(2), Some(Duration::from_secs(2)));
    assert_eq!(
        auth_park_probe_override(600),
        Some(Duration::from_secs(600))
    );
}

#[test]
fn temp_file_name_is_unique() {
    // Even with identical parameters called in the same millisecond,
    // the atomic counter ensures unique names.
    let a = temp_file_name("metadata", "session-abc123", 0);
    let b = temp_file_name("metadata", "session-abc123", 0);
    assert_ne!(
        a, b,
        "temp file names should be unique (counter suffix differs)"
    );
}

#[test]
fn temp_file_name_contains_components() {
    let name = temp_file_name("config", "019abc-def0-1234", 3);
    assert!(name.contains("turn3"), "should contain turn number");
    assert!(name.contains("config"), "should contain artifact name");
}

#[test]
fn with_client_version_sets_field() {
    // with_client_version() must propagate the version onto every enqueued item
    // so the gcs_queue_upload span carries it for analytics dashboard breakdowns.
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);

    let queue = UploadQueue {
        tx,
        queue_dir,
        resolver: Arc::new(MockResolver),
        stats,
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    assert!(queue.client_version.is_none(), "starts as None");

    let queue = queue.with_client_version("1.2.3-test");
    assert_eq!(
        queue.client_version.as_deref(),
        Some("1.2.3-test"),
        "with_client_version sets the field"
    );
}

#[tokio::test]
async fn enqueue_copies_client_version_onto_item() {
    // Items enqueued after with_client_version() must carry the version,
    // which the worker reads to stamp the gcs_queue_upload span.
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);

    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats,
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: Some("0.1.42".to_string()),
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    queue
        .enqueue(
            b"data",
            "session/turn_0/test.json",
            "application/json",
            "test",
            "session-123",
            0,
        )
        .await
        .unwrap();

    let item = rx.recv().await.expect("item enqueued");
    assert_eq!(
        item.client_version.as_deref(),
        Some("0.1.42"),
        "enqueued item carries client_version from the queue"
    );
}

/// Build a worker-less queue (no spawned worker; caller owns `rx`) for the
/// `enqueue_bytes_blocking` outcome tests. Mirrors the inline literals used
/// by the other unit tests above.
fn build_test_queue(
    queue_dir: PathBuf,
    tx: mpsc::Sender<UploadQueueItem>,
    stats: Arc<UploadQueueStats>,
    max_queue_bytes: u64,
) -> UploadQueue {
    UploadQueue {
        tx,
        queue_dir,
        resolver: Arc::new(MockResolver),
        stats,
        max_queue_bytes,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    }
}

#[tokio::test]
async fn enqueue_bytes_blocking_returns_enqueued_on_happy_path() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let outcome = queue
        .enqueue_bytes_blocking(
            b"archive-bytes",
            "sess/turn_0/before_changes.tar.gz",
            "application/gzip",
            "before_changes",
            "session-xyz",
            0,
        )
        .await;

    assert_eq!(outcome, EnqueueOutcome::Enqueued);
    // The worker channel received exactly one item (durable hand-off).
    let item = rx.recv().await.expect("item should be enqueued");
    assert_eq!(item.gcs_path, "sess/turn_0/before_changes.tar.gz");
    assert_eq!(stats.enqueued.load(Ordering::Relaxed), 1);
    assert_eq!(stats.enqueue_fallbacks.load(Ordering::Relaxed), 0);
    // While the item is alive the queue dir holds the temp archive and its
    // recovery sidecar.
    let mut names: Vec<String> = std::fs::read_dir(&queue_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names.len(),
        2,
        "temp + sidecar written to queue dir: {names:?}"
    );
    assert_eq!(
        names.iter().filter(|n| n.ends_with(SIDECAR_SUFFIX)).count(),
        1,
        "exactly one sidecar manifest accompanies the temp file"
    );
}

#[tokio::test]
async fn enqueue_dedups_identical_gcs_path_until_item_settles() {
    // Holding `rx` keeps the first item (and its in-flight mark) alive across enqueues.
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let blob = "changes_dedup/v2/blobs/sha256_aaa";

    let first = queue
        .enqueue_bytes_blocking(
            b"video",
            blob,
            "application/octet-stream",
            "dedup_aaa",
            "s",
            0,
        )
        .await;
    assert_eq!(first, EnqueueOutcome::Enqueued);

    let dup = queue
        .enqueue_bytes_blocking(
            b"video",
            blob,
            "application/octet-stream",
            "dedup_aaa",
            "s",
            1,
        )
        .await;
    assert_eq!(dup, EnqueueOutcome::Deduplicated);
    assert_eq!(stats.deduplicated.load(Ordering::Relaxed), 1);

    let other = queue
        .enqueue_bytes_blocking(
            b"other",
            "changes_dedup/v2/blobs/sha256_bbb",
            "application/octet-stream",
            "dedup_bbb",
            "s",
            1,
        )
        .await;
    assert_eq!(other, EnqueueOutcome::Enqueued);

    let temp_files = std::fs::read_dir(&queue_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(SIDECAR_SUFFIX))
        .count();
    assert_eq!(temp_files, 2, "duplicate must not spill a second copy");

    // Dropping the buffered item un-marks it in-flight, as a worker terminal would.
    let first_item = rx.recv().await.expect("first item buffered");
    assert_eq!(first_item.gcs_path, blob);
    drop(first_item);

    let after_settle = queue
        .enqueue_bytes_blocking(
            b"video",
            blob,
            "application/octet-stream",
            "dedup_aaa",
            "s",
            2,
        )
        .await;
    assert_eq!(
        after_settle,
        EnqueueOutcome::Enqueued,
        "re-enqueue allowed once the in-flight copy settled"
    );
}

#[tokio::test]
async fn non_content_addressed_path_is_never_deduped() {
    // A stable / turn-keyed path carries mutable content, so a changed
    // re-upload of the same path must go through rather than be dropped.
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    // Stable path re-emitted with new content (cf. workspace_tool_definitions.json).
    let path = "s/workspace_tool_definitions.json";

    let first = queue
        .enqueue_bytes_blocking(b"v1", path, "application/json", "tools", "s", 0)
        .await;
    assert_eq!(first, EnqueueOutcome::Enqueued);

    // Same path, newer bytes, while the first is still in flight (rx holds it).
    let second = queue
        .enqueue_bytes_blocking(b"v2-updated", path, "application/json", "tools", "s", 1)
        .await;
    assert_eq!(
        second,
        EnqueueOutcome::Enqueued,
        "mutable-content re-upload on a stable path must not be dropped"
    );
    assert_eq!(stats.deduplicated.load(Ordering::Relaxed), 0);

    // Both uploads spilled a temp file, so the newer content was not dropped.
    let temp_files = std::fs::read_dir(&queue_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(SIDECAR_SUFFIX))
        .count();
    assert_eq!(temp_files, 2, "both uploads must be queued (no path dedup)");

    let _ = rx.recv().await;
    let _ = rx.recv().await;
}

#[tokio::test]
async fn enqueue_file_reference_dedups_before_snapshotting() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("video.bin");
    std::fs::write(&source, b"reference-bytes").unwrap();
    let sha = crate::sha256_hex_from_file(&source, None).unwrap();
    let blob = format!("changes_dedup/v2/blobs/sha256_{sha}");

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    // Do not await the first completion; nothing drains it here.
    let _first = queue
        .enqueue_file_reference(
            &source,
            &sha,
            &blob,
            "application/octet-stream",
            "dedup",
            "s",
            0,
        )
        .await
        .expect("first reference enqueues");

    let dup = queue
        .enqueue_file_reference(
            &source,
            &sha,
            &blob,
            "application/octet-stream",
            "dedup",
            "s",
            1,
        )
        .await
        .expect("dup reference returns Ok");
    let dup_result = dup.completion_rx.await.expect("completion resolves");
    assert!(
        dup_result.is_err(),
        "deduplicated reference resolves non-fatally"
    );
    assert_eq!(stats.deduplicated.load(Ordering::Relaxed), 1);

    let snapshots = std::fs::read_dir(&queue_dir).unwrap().count();
    assert_eq!(snapshots, 1, "duplicate reference must not snapshot again");

    let _ = rx.recv().await;
}

#[tokio::test]
async fn enqueue_file_dedups_identical_gcs_path() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("blob.bin");
    std::fs::write(&source, b"file-bytes").unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );
    let blob = "changes_dedup/v2/blobs/sha256_file";

    queue
        .enqueue_file(
            &source,
            blob,
            "application/octet-stream",
            "dedup_file",
            "s",
            0,
        )
        .await
        .unwrap();
    queue
        .enqueue_file(
            &source,
            blob,
            "application/octet-stream",
            "dedup_file",
            "s",
            1,
        )
        .await
        .unwrap();

    assert_eq!(stats.deduplicated.load(Ordering::Relaxed), 1);
    let copies = std::fs::read_dir(&queue_dir).unwrap().count();
    assert_eq!(
        copies, 1,
        "duplicate enqueue_file must not copy a second time"
    );
}

#[tokio::test]
async fn enqueue_file_blocking_dedup_resolves_completion() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("blob.bin");
    std::fs::write(&source, b"file-bytes").unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );
    let blob = "changes_dedup/v2/blobs/sha256_fb";

    let _first = queue
        .enqueue_file_blocking(
            &source,
            blob,
            "application/octet-stream",
            "dedup_fb",
            "s",
            0,
            false,
        )
        .await
        .expect("first enqueues");
    let dup = queue
        .enqueue_file_blocking(
            &source,
            blob,
            "application/octet-stream",
            "dedup_fb",
            "s",
            1,
            false,
        )
        .await
        .expect("dup returns Ok");

    let dup_result = dup.completion_rx.await.expect("completion resolves");
    assert!(
        dup_result.is_err(),
        "deduplicated enqueue_file_blocking resolves non-fatally"
    );
    assert_eq!(stats.deduplicated.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn enqueue_bytes_blocking_falls_back_to_inline_when_over_budget() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    // max_queue_bytes = 0 makes any non-empty content exceed the budget.
    let queue = build_test_queue(queue_dir.clone(), tx, stats.clone(), 0);

    let outcome = queue
        .enqueue_bytes_blocking(
            b"too-big",
            "sess/turn_1/after_changes.tar.gz",
            "application/gzip",
            "after_changes",
            "session-xyz",
            1,
        )
        .await;

    assert_eq!(outcome, EnqueueOutcome::FellBackToInline);
    assert_eq!(stats.enqueue_fallbacks.load(Ordering::Relaxed), 1);
    // The over-budget path removes its temp file before falling back inline.
    let entries = std::fs::read_dir(&queue_dir).unwrap().count();
    assert_eq!(entries, 0, "temp file removed on over-budget fallback");
}

#[tokio::test]
async fn enqueue_bytes_blocking_returns_failed_when_worker_closed() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    drop(rx); // close the worker channel — try_send will return Closed.
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let outcome = queue
        .enqueue_bytes_blocking(
            b"bytes",
            "sess/turn_2/after_changes.tar.gz",
            "application/gzip",
            "after_changes",
            "session-xyz",
            2,
        )
        .await;

    assert!(
        matches!(outcome, EnqueueOutcome::Failed { .. }),
        "closed worker channel must map to Failed, got {outcome:?}"
    );
    // A closed channel is a true failure: no inline fallback is spawned.
    assert_eq!(stats.enqueue_fallbacks.load(Ordering::Relaxed), 0);
    // Pending counters are rolled back and the temp file removed.
    assert_eq!(stats.pending.load(Ordering::Relaxed), 0);
    assert_eq!(stats.pending_bytes.load(Ordering::Relaxed), 0);
    let entries = std::fs::read_dir(&queue_dir).unwrap().count();
    assert_eq!(entries, 0, "temp file removed when the worker is closed");
}

#[tokio::test]
async fn enqueue_bytes_blocking_returns_failed_when_temp_write_fails() {
    // A queue dir nested under a non-existent path makes write_temp_file fail.
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("does/not/exist");

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(queue_dir, tx, stats.clone(), DEFAULT_MAX_QUEUE_BYTES);

    let outcome = queue
        .enqueue_bytes_blocking(
            b"bytes",
            "sess/turn_3/after_changes.tar.gz",
            "application/gzip",
            "after_changes",
            "session-xyz",
            3,
        )
        .await;

    assert!(
        matches!(outcome, EnqueueOutcome::Failed { .. }),
        "temp-write failure must map to Failed, got {outcome:?}"
    );
    // Nothing was handed off to the worker.
    assert_eq!(stats.enqueued.load(Ordering::Relaxed), 0);
    assert_eq!(stats.enqueue_fallbacks.load(Ordering::Relaxed), 0);
}

/// `enqueue_bytes_blocking` writes a temp+sidecar pair whose fields
/// describe the bytes, and stamps the sidecar path onto the item.
#[tokio::test]
async fn enqueue_bytes_blocking_writes_sidecar_manifest_alongside_tmp() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let content = b"archive-bytes-payload";
    let outcome = queue
        .enqueue_bytes_blocking(
            content,
            "session-xyz/turn_7/before_changes.tar.gz",
            "application/gzip",
            "before_changes.tar.gz",
            "session-xyz",
            7,
        )
        .await;
    assert_eq!(outcome, EnqueueOutcome::Enqueued);

    let mut names: Vec<String> = std::fs::read_dir(&queue_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names.len(),
        2,
        "temp + sidecar written as a pair: {names:?}"
    );
    let sidecar_name = names
        .iter()
        .find(|n| n.ends_with(SIDECAR_SUFFIX))
        .expect("a .meta.json sidecar was written")
        .clone();
    let temp_name = names
        .iter()
        .find(|n| !n.ends_with(SIDECAR_SUFFIX))
        .expect("the archive temp file was written")
        .clone();
    assert_eq!(sidecar_name, format!("{temp_name}{SIDECAR_SUFFIX}"));

    let item = rx.recv().await.expect("item handed to the worker");
    assert_eq!(
        item.sidecar_path.as_ref().unwrap(),
        &queue_dir.join(&sidecar_name)
    );

    let raw = std::fs::read(queue_dir.join(&sidecar_name)).unwrap();
    let sidecar: QueueItemSidecar = serde_json::from_slice(&raw).unwrap();
    assert_eq!(sidecar.schema_version, QUEUE_ITEM_SIDECAR_SCHEMA_VERSION);
    assert_eq!(sidecar.session_id, "session-xyz");
    assert_eq!(sidecar.turn_number, 7);
    assert_eq!(sidecar.gcs_path, "session-xyz/turn_7/before_changes.tar.gz");
    assert_eq!(sidecar.content_type, "application/gzip");
    assert_eq!(sidecar.artifact_name, "before_changes.tar.gz");
    assert_eq!(sidecar.sha256, crate::sha256_hex(content));
    assert!(!sidecar.enqueued_at.is_empty(), "enqueued_at timestamp set");
}

/// The fire-and-forget `enqueue` keeps the legacy single-temp-file shape:
/// no sidecar written, no sidecar path on the item.
#[tokio::test]
async fn enqueue_does_not_write_sidecar_legacy_fast_path() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = build_test_queue(
        queue_dir.clone(),
        tx,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    queue
        .enqueue(
            b"legacy-bytes",
            "session-xyz/turn_0/metadata.json",
            "application/json",
            "metadata.json",
            "session-xyz",
            0,
        )
        .await
        .unwrap();

    let names: Vec<String> = std::fs::read_dir(&queue_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.len(),
        1,
        "exactly one temp file, no sidecar: {names:?}"
    );
    assert!(
        !names[0].ends_with(SIDECAR_SUFFIX),
        "legacy enqueue must not write a .meta.json sidecar"
    );
    let item = rx.recv().await.expect("item handed to the worker");
    assert!(
        item.sidecar_path.is_none(),
        "legacy enqueue item carries no sidecar path"
    );
}

#[test]
fn stats_initial_values() {
    let stats = UploadQueueStats::new();
    assert_eq!(stats.pending.load(Ordering::Relaxed), 0);
    assert_eq!(stats.pending_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(stats.enqueued.load(Ordering::Relaxed), 0);
    assert_eq!(stats.uploaded.load(Ordering::Relaxed), 0);
    assert_eq!(stats.failed.load(Ordering::Relaxed), 0);
    assert_eq!(stats.circuit_breaker_trips.load(Ordering::Relaxed), 0);
    assert_eq!(stats.enqueue_fallbacks.load(Ordering::Relaxed), 0);
    assert_eq!(stats.leaked_temp_files.load(Ordering::Relaxed), 0);
    assert_eq!(stats.reference_stale.load(Ordering::Relaxed), 0);
    assert_eq!(stats.cleanup_orphan_mismatched.load(Ordering::Relaxed), 0);
}

#[test]
fn over_disk_budget_respects_limit() {
    let stats = Arc::new(UploadQueueStats::new());
    stats.pending_bytes.store(7_000_000_000, Ordering::Relaxed); // 7 GB

    // Queue with 8 GB budget
    let queue = UploadQueue {
        tx: mpsc::channel(1).0,
        queue_dir: PathBuf::from("/tmp"),
        resolver: Arc::new(MockResolver),
        stats,
        max_queue_bytes: 8_000_000_000,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    // 500 MB more is under budget
    assert!(!queue.over_disk_budget(500_000_000));
    // 1.5 GB more exceeds budget
    assert!(queue.over_disk_budget(1_500_000_000));
}

#[test]
fn cleanup_orphans_removes_old_files() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    // Create a "stale" file and set its mtime to 2 hours ago.
    let stale = queue_dir.join("stale_file.json");
    std::fs::write(&stale, b"old data").unwrap();
    let two_hours_ago = std::time::SystemTime::now() - Duration::from_secs(7200);
    let times = std::fs::FileTimes::new().set_modified(two_hours_ago);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_times(times)
        .unwrap();

    // Create a "fresh" file (mtime = now).
    let fresh = queue_dir.join("fresh_file.json");
    std::fs::write(&fresh, b"new data").unwrap();

    let queue = UploadQueue {
        tx: mpsc::channel(1).0,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: Arc::new(UploadQueueStats::new()),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    // Clean up files older than 1 hour.
    queue.cleanup_orphans(Duration::from_secs(3600));

    assert!(!stale.exists(), "stale file should be deleted");
    assert!(fresh.exists(), "fresh file should be kept");
}

#[test]
fn cleanup_orphans_removes_stale_directories() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    // Stale non-scratch subdir: should be removed as a whole.
    let stale_dir = queue_dir.join("other_stale");
    std::fs::create_dir_all(&stale_dir).unwrap();
    std::fs::write(stale_dir.join("a.txt"), b"old").unwrap();
    let three_hours_ago = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
    let ft = filetime::FileTime::from_system_time(three_hours_ago);
    filetime::set_file_mtime(&stale_dir, ft).unwrap();

    // Create a fresh subdirectory that should be preserved.
    let fresh_dir = queue_dir.join("scratch_fresh");
    std::fs::create_dir_all(&fresh_dir).unwrap();
    std::fs::write(fresh_dir.join("data.txt"), b"keep me").unwrap();

    // Also create a stale file to verify mixed cleanup still works.
    let stale_file = queue_dir.join("stale.gz");
    std::fs::write(&stale_file, b"old").unwrap();
    filetime::set_file_mtime(&stale_file, ft).unwrap();

    let queue = UploadQueue {
        tx: mpsc::channel(1).0,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: Arc::new(UploadQueueStats::new()),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    queue.cleanup_orphans(Duration::from_secs(3600));

    assert!(
        !stale_dir.exists(),
        "stale non-scratch directory tree should be removed"
    );
    assert!(!stale_file.exists(), "stale file should be removed");
    assert!(fresh_dir.exists(), "fresh directory should be preserved");
    assert!(
        fresh_dir.join("data.txt").exists(),
        "files inside fresh directory should be preserved"
    );
}

/// Stale `scratch/<sid>/` is reaped; `scratch/` and fresh siblings survive.
#[test]
fn cleanup_orphans_recurses_into_scratch_subdirs() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    let scratch_dir = queue_dir.join("scratch");
    std::fs::create_dir_all(&scratch_dir).unwrap();

    // Stale session subdir under scratch/.
    let stale_session = scratch_dir.join("old-session-abc");
    std::fs::create_dir_all(&stale_session).unwrap();
    std::fs::write(stale_session.join("pre_edit.txt"), b"old copy").unwrap();
    let three_hours_ago = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
    let ft = filetime::FileTime::from_system_time(three_hours_ago);
    filetime::set_file_mtime(&stale_session, ft).unwrap();

    // Fresh session subdir under scratch/ — must survive.
    let fresh_session = scratch_dir.join("fresh-session-xyz");
    std::fs::create_dir_all(&fresh_session).unwrap();
    std::fs::write(fresh_session.join("hot.txt"), b"keep").unwrap();
    let now = std::time::SystemTime::now();
    let fresh_ft = filetime::FileTime::from_system_time(now);
    filetime::set_file_mtime(&fresh_session, fresh_ft).unwrap();

    // scratch/ has a fresh mtime (a new session just landed) so the old
    // top-level age check would have skipped it entirely.

    let queue = UploadQueue {
        tx: mpsc::channel(1).0,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: Arc::new(UploadQueueStats::new()),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    queue.cleanup_orphans(Duration::from_secs(3600));

    assert!(
        scratch_dir.exists(),
        "scratch/ itself must be preserved across sweeps"
    );
    assert!(
        !stale_session.exists(),
        "stale scratch/<sid>/ subdir should be removed"
    );
    assert!(
        fresh_session.exists(),
        "fresh scratch/<sid>/ subdir should be preserved"
    );
    assert!(
        fresh_session.join("hot.txt").exists(),
        "files inside fresh session subdir should be preserved"
    );
}

#[tokio::test]
async fn enqueue_writes_temp_file_and_returns_ok() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);

    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    let result = queue
        .enqueue(
            b"test content",
            "session/turn_0/config.json",
            "application/json",
            "config",
            "session-123",
            0,
        )
        .await;

    assert!(result.is_ok());
    assert_eq!(stats.pending.load(Ordering::Relaxed), 1);
    assert!(stats.pending_bytes.load(Ordering::Relaxed) > 0);

    // Verify temp file was written.
    let files: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let content = std::fs::read(files[0].path()).unwrap();
    assert_eq!(content, b"test content");
}

#[tokio::test]
async fn enqueue_file_copies_to_queue() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let source = temp.path().join("source.tar.gz");
    std::fs::write(&source, b"tarball bytes").unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);

    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    let result = queue
        .enqueue_file(
            &source,
            "session/turn_0/repo_changes.tar.gz",
            "application/gzip",
            "repo_changes",
            "session-456",
            0,
        )
        .await;

    assert!(result.is_ok());
    assert_eq!(stats.pending.load(Ordering::Relaxed), 1);

    // Verify file exists in queue dir.
    let files: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let content = std::fs::read(files[0].path()).unwrap();
    assert_eq!(content, b"tarball bytes");
}

#[tokio::test]
async fn enqueue_file_blocking_returns_receiver_and_copies() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let source = temp.path().join("blob.bin");
    std::fs::write(&source, b"dedup blob content").unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);

    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    let result = queue
        .enqueue_file_blocking(
            &source,
            "changes_dedup/v2/blobs/sha256_abc123",
            "application/octet-stream",
            "dedup_abc123",
            "session-789",
            1,
            false,
        )
        .await;

    assert!(result.is_ok());
    let enqueue_result = result.unwrap();
    assert_eq!(enqueue_result.original_size, 18);

    assert_eq!(stats.pending.load(Ordering::Relaxed), 1);
    assert!(stats.pending_bytes.load(Ordering::Relaxed) > 0);

    let files: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let content = std::fs::read(files[0].path()).unwrap();
    assert_eq!(content, b"dedup blob content");
    // B9 copy-path: source outside queue_dir must be preserved by the
    // copy fallback (catches a regression that unconditionally renamed).
    assert!(
        source.exists(),
        "outside-queue source must be preserved (copy fallback)"
    );
}

#[tokio::test]
async fn enqueue_file_blocking_stores_plain_file_even_with_compress_true() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let source = temp.path().join("big.txt");
    let content = "hello world, this is compressible text!\n".repeat(30);
    std::fs::write(&source, &content).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);

    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    let result = queue
        .enqueue_file_blocking(
            &source,
            "patches/sha256_abc",
            "application/octet-stream",
            "patches_abc",
            "session-comp",
            0,
            true,
        )
        .await
        .unwrap();

    assert_eq!(result.original_size, content.len() as u64);

    // The queued file on disk is the ORIGINAL (uncompressed) — compression
    // happens at upload time in the worker, not at enqueue time.
    let files: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1);
    let queued = std::fs::read(files[0].path()).unwrap();
    assert_eq!(queued.len(), content.len());

    // The item carries compress=true for the worker to act on.
    let item = rx.recv().await.expect("item enqueued");
    assert!(item.compress);
}

/// Sources already in `queue_dir` are renamed (not copied) — no double-on-disk.
#[tokio::test]
async fn enqueue_file_blocking_renames_when_source_inside_queue_dir() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    // Source lives inside queue_dir — the rename branch should fire.
    let source = queue_dir.join("dedup_abc_0_0");
    std::fs::write(&source, b"dedup blob content").unwrap();

    // Pre-call inode — preserved by `rename(2)`; a copy+remove would change it.
    #[cfg(unix)]
    let src_inode = {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(&source).unwrap().ino()
    };

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);

    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    queue
        .enqueue_file_blocking(
            &source,
            "changes_dedup/v2/blobs/sha256_abc",
            "application/octet-stream",
            "dedup_abc",
            "session-rename",
            1,
            false,
        )
        .await
        .unwrap();

    assert!(
        !source.exists(),
        "source file inside queue_dir must be moved, not copied"
    );
    // Exactly one file remains in queue_dir: the renamed dest.
    let files: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert_eq!(files.len(), 1, "expected one file after rename");
    assert_eq!(
        std::fs::read(files[0].path()).unwrap(),
        b"dedup blob content"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let dest_inode = std::fs::metadata(files[0].path()).unwrap().ino();
        assert_eq!(
            src_inode, dest_inode,
            "rename(2) preserves inode; a copy+remove regression would allocate a new inode"
        );
    }
}

/// When both rename and copy fail, source is preserved and Err is returned.
#[test]
fn move_or_copy_to_queue_rename_then_copy_failure_keeps_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let source = queue_dir.join("dedup_src_0_0");
    std::fs::write(&source, b"payload").unwrap();

    // Dest is a directory — both rename and copy fail.
    let dest = queue_dir.join("dedup_src_dest");
    std::fs::create_dir(&dest).unwrap();
    std::fs::write(dest.join("blocker"), b"x").unwrap();

    let stats = UploadQueueStats::new();
    let result = move_or_copy_to_queue(&source, &dest, &queue_dir, &stats);
    assert!(result.is_err(), "rename+copy onto a directory must fail");
    assert!(source.exists(), "source must remain on rename+copy failure");
}

/// Budget gate diverts to inline upload: no staging, `enqueue_fallbacks`
/// bumps, `pending_bytes` unchanged.
#[tokio::test]
async fn enqueue_file_blocking_budget_gate_fallback() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    // 200 bytes exceeds the 100-byte headroom we configure below.
    let source = temp.path().join("src.bin");
    std::fs::write(&source, vec![0xCD; 200]).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let max_queue_bytes: u64 = 1000;
    stats
        .pending_bytes
        .store(max_queue_bytes - 100, Ordering::Relaxed);
    let pre_pending = stats.pending_bytes.load(Ordering::Relaxed);

    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    let result = queue
        .enqueue_file_blocking(
            &source,
            "gcs/path",
            "application/octet-stream",
            "dedup_x",
            "session-budget",
            0,
            false,
        )
        .await
        .expect("budget fallback must return Ok(EnqueueResult)");

    // Mock resolver isn't a real GCS; we only assert flow invariants below.
    let _ = result.completion_rx.await;

    let staged: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert!(
        staged.is_empty(),
        "budget fallback must NOT stage a temp file in queue_dir"
    );
    assert_eq!(
        stats.enqueue_fallbacks.load(Ordering::Relaxed),
        1,
        "budget gate must bump enqueue_fallbacks exactly once"
    );
    assert_eq!(
        stats.pending_bytes.load(Ordering::Relaxed),
        pre_pending,
        "budget fallback must NOT bump pending_bytes"
    );
    assert!(
        source.exists(),
        "source must NOT be moved on the fallback path"
    );
}

/// Rename-fail / copy-succeed in same-dir: source is removed by the
/// post-copy `try_remove_temp` so we don't hold two copies.
#[test]
fn move_or_copy_to_queue_rename_fail_copy_succeed_removes_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let source = queue_dir.join("dedup_src_0_0");
    std::fs::write(&source, b"payload").unwrap();
    let dest = queue_dir.join("dedup_src_dest");

    let stats = UploadQueueStats::new();
    // Injected rename fails; the real copy runs and succeeds.
    let result = move_or_copy_to_queue_with(
        &source,
        &dest,
        &queue_dir,
        &stats,
        |_, _| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "forced rename failure",
            ))
        },
        copy_to_queue,
    );

    assert!(result.is_ok(), "rename-fail + copy-succeed must return Ok");
    assert!(
        !source.exists(),
        "source must be removed via try_remove_temp after copy succeeds"
    );
    assert!(dest.exists(), "dest must contain the copied payload");
    assert_eq!(std::fs::read(&dest).unwrap(), b"payload");
    assert_eq!(
        stats.leaked_temp_files.load(Ordering::Relaxed),
        0,
        "successful try_remove_temp must NOT bump leaked_temp_files"
    );
}

/// `try_remove_temp` bumps the counter on real errors but stays silent on `NotFound`.
#[test]
fn try_remove_temp_bumps_counter_on_real_error_but_not_notfound() {
    let stats = Arc::new(UploadQueueStats::new());

    // NotFound: counter unchanged.
    let missing = PathBuf::from("/definitely/does/not/exist/leaked-temp.bin");
    try_remove_temp(&missing, Some(&stats));
    assert_eq!(
        stats.leaked_temp_files.load(Ordering::Relaxed),
        0,
        "NotFound must not bump leaked_temp_files"
    );

    // `remove_file` on a directory fails with a non-NotFound error.
    let temp = tempfile::TempDir::new().unwrap();
    let dir_as_file = temp.path().join("a_directory");
    std::fs::create_dir(&dir_as_file).unwrap();
    try_remove_temp(&dir_as_file, Some(&stats));
    assert_eq!(
        stats.leaked_temp_files.load(Ordering::Relaxed),
        1,
        "real (non-NotFound) errors must bump leaked_temp_files"
    );
    assert!(dir_as_file.exists());

    // Same shape, but `None` stats must not touch the counter.
    let dir2 = temp.path().join("a_directory_2");
    std::fs::create_dir(&dir2).unwrap();
    let prev = stats.leaked_temp_files.load(Ordering::Relaxed);
    try_remove_temp(&dir2, None);
    assert_eq!(
        stats.leaked_temp_files.load(Ordering::Relaxed),
        prev,
        "None stats arg must NOT touch the counter"
    );
    assert!(dir2.exists(), "directory should still be present");
}

#[tokio::test]
async fn counting_reader_tracks_bytes() {
    use tokio::io::AsyncReadExt;

    let data = b"hello world, counting reader test data";
    let reader = &data[..];
    let counter = Arc::new(AtomicU64::new(0));
    let mut counting = CountingReader {
        inner: reader,
        bytes_read: counter.clone(),
    };
    let mut buf = Vec::new();
    counting.read_to_end(&mut buf).await.unwrap();
    assert_eq!(buf, data);
    assert_eq!(counter.load(Ordering::Relaxed), data.len() as u64);
}

#[tokio::test]
async fn streaming_zstd_produces_valid_compressed_output() {
    use async_compression::tokio::bufread::ZstdDecoder;
    use tokio::io::AsyncReadExt;

    let content = "hello world, this is compressible text!\n".repeat(30);
    let reader = tokio::io::BufReader::new(content.as_bytes());
    let encoder = ZstdEncoder::new(reader);
    let counter = Arc::new(AtomicU64::new(0));
    let mut counting = CountingReader {
        inner: encoder,
        bytes_read: counter.clone(),
    };
    let mut compressed = Vec::new();
    counting.read_to_end(&mut compressed).await.unwrap();

    // Verify zstd magic bytes
    assert_eq!(&compressed[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
    // Counter matches actual compressed size
    assert_eq!(counter.load(Ordering::Relaxed), compressed.len() as u64);
    // Compressed is smaller than original
    assert!(compressed.len() < content.len());

    // Roundtrip: decompress and verify
    let mut decoder = ZstdDecoder::new(tokio::io::BufReader::new(&compressed[..]));
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed).await.unwrap();
    assert_eq!(decompressed, content.as_bytes());
}

#[test]
fn compress_decision_size_threshold() {
    let decide = |compress: bool, size: u64| -> bool { compress && size >= COMPRESS_MIN_BYTES };

    assert!(decide(true, 128));
    assert!(decide(true, 1000));
    assert!(!decide(true, 127));
    assert!(!decide(true, 1));
    assert!(!decide(false, 1000));
    assert!(!decide(false, 128));
}

#[tokio::test]
async fn streaming_zstd_handles_incompressible_data() {
    use tokio::io::AsyncReadExt;

    // Pseudo-random data that won't compress well
    let mut rng: u64 = 0xDEAD_BEEF_CAFE_BABE;
    let content: Vec<u8> = (0..1024)
        .map(|_| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng as u8
        })
        .collect();

    let reader = tokio::io::BufReader::new(&content[..]);
    let encoder = ZstdEncoder::new(reader);
    let counter = Arc::new(AtomicU64::new(0));
    let mut counting = CountingReader {
        inner: encoder,
        bytes_read: counter.clone(),
    };
    let mut compressed = Vec::new();
    counting.read_to_end(&mut compressed).await.unwrap();

    // Zstd header is valid even for incompressible data
    assert_eq!(&compressed[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
    // Counter matches
    assert_eq!(counter.load(Ordering::Relaxed), compressed.len() as u64);
}

struct CountingResolver {
    count: Arc<AtomicU32>,
    proxy_base_url: String,
}

impl TraceExportSource for CountingResolver {
    fn resolve(&self) -> TraceExportConfig {
        self.count.fetch_add(1, Ordering::SeqCst);
        TraceExportConfig {
            bucket_url: None,
            service_account_key: None,
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
            upload_method: UploadMethod::Proxy {
                proxy_base_url: self.proxy_base_url.clone(),
                user_token: "test-token".to_string(),
                deployment_key: None,
                alpha_test_key: None,
            },
        }
    }
}

#[test]
fn non_retryable_error_detects_proxy_401() {
    let err = anyhow::anyhow!("Upload to 'path': HTTP 401 - Unauthorized");
    assert!(is_non_retryable_error(&err));
}

#[test]
fn non_retryable_error_detects_proxy_403() {
    let err = anyhow::anyhow!("Upload to 'path': HTTP 403 - Forbidden");
    assert!(is_non_retryable_error(&err));
}

#[test]
fn non_retryable_error_detects_direct_mode_errors() {
    assert!(is_non_retryable_error(&anyhow::anyhow!("401 Unauthorized")));
    assert!(is_non_retryable_error(&anyhow::anyhow!("403 Forbidden")));
}

#[test]
fn non_retryable_error_ignores_retryable_statuses() {
    assert!(!is_non_retryable_error(&anyhow::anyhow!(
        "HTTP 429 - Too Many Requests"
    )));
    assert!(!is_non_retryable_error(&anyhow::anyhow!(
        "HTTP 500 - Internal Server Error"
    )));
    assert!(!is_non_retryable_error(&anyhow::anyhow!(
        "HTTP 503 - Service Unavailable"
    )));
}

#[test]
fn non_retryable_error_ignores_network_errors() {
    assert!(!is_non_retryable_error(&anyhow::anyhow!(
        "Connection refused"
    )));
    assert!(!is_non_retryable_error(&anyhow::anyhow!(
        "DNS resolution failed"
    )));
    assert!(!is_non_retryable_error(&anyhow::anyhow!("timeout")));
}

#[test]
fn non_retryable_error_detects_chained_errors() {
    let inner = anyhow::anyhow!("HTTP 401 - token expired");
    let outer = inner.context("Streaming upload failed for session/turn_0/metadata.json");
    assert!(is_non_retryable_error(&outer));
}

fn http_err(status_code: u16) -> anyhow::Error {
    HttpUploadError {
        status_code,
        message: format!("op: HTTP {status_code}"),
    }
    .into()
}

#[test]
fn upload_disposition_structured_terminal() {
    // 525/526: origin TLS never clears on its own — the queue must drop
    // instead of re-uploading until max_age.
    for code in [400u16, 403, 404, 525, 526] {
        assert_eq!(upload_disposition(&http_err(code)), Disposition::Terminal);
        // Reachable through gcs.rs `.with_context` wrapping.
        let wrapped = http_err(code).context("Streaming upload failed for s/turn_0/x");
        assert_eq!(upload_disposition(&wrapped), Disposition::Terminal);
    }
}

#[test]
fn upload_disposition_structured_auth_and_retryable() {
    assert_eq!(upload_disposition(&http_err(401)), Disposition::AuthRefresh);
    for code in [429u16, 500, 503, 522] {
        assert_eq!(upload_disposition(&http_err(code)), Disposition::Retryable);
    }
}

#[test]
fn upload_disposition_unstructured_is_not_terminal() {
    // Classification of terminal status is purely structural: a
    // non-`HttpUploadError` whose text merely contains "HTTP 404" (e.g. a 5xx
    // body) must NOT be terminal — this is the false-positive the structured
    // check exists to prevent.
    assert_eq!(
        upload_disposition(&anyhow::anyhow!(
            "HTTP 503 - upstream said HTTP 404 Not Found"
        )),
        Disposition::Retryable
    );
    // Generic transport errors are retried, not dropped or auth-refreshed.
    assert_eq!(
        upload_disposition(&anyhow::anyhow!("Connection reset")),
        Disposition::Retryable
    );
}

#[test]
fn upload_disposition_breaker_open_is_retryable() {
    // Breaker-open short-circuits surface a structured 503 so they retry
    // with backoff rather than triggering a credential refresh.
    let err: anyhow::Error = HttpUploadError {
        status_code: 503,
        message: "upload: circuit breaker open; retry after 1.0s".to_string(),
    }
    .into();
    assert_eq!(upload_disposition(&err), Disposition::Retryable);
}

#[test]
fn upload_disposition_direct_mode_auth_fallback() {
    // Direct-mode (gcloud) errors are unstructured strings; the 401/403
    // message scrape routes them to a credential refresh.
    assert_eq!(
        upload_disposition(&anyhow::anyhow!("403 Forbidden")),
        Disposition::AuthRefresh
    );
}

#[tokio::test]
async fn upload_with_retries_resolves_credentials_each_attempt() {
    let count = Arc::new(AtomicU32::new(0));
    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: count.clone(),
        proxy_base_url: "http://127.0.0.1:1".to_string(),
    });

    let policy = UploadRetryPolicy {
        max_attempts: 3,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        multiplier: 1.0,
        max_age: DEFAULT_MAX_AGE,
        auth_park_probe_interval: DEFAULT_AUTH_PARK_PROBE_INTERVAL,
    };

    let mut item = UploadQueueItem {
        source: UploadSource::OwnedTemp(PathBuf::from("/nonexistent/upload_queue_test_file")),
        gcs_path: "test/path".to_string(),
        content_type: "application/json".to_string(),
        artifact_name: "test".to_string(),
        attempts: 0,
        enqueued_at: Instant::now(),
        sidecar_path: None,
        completion_tx: None,
        client_version: None,
        compress: false,
        parent_span: tracing::Span::none(),
        _in_flight: None,
    };

    let result = run_upload_with_retries(&mut item, &resolver, &policy).await;
    assert!(result.is_err());
    assert_eq!(item.attempts, 3, "should exhaust all retry attempts");
    assert_eq!(
        count.load(Ordering::SeqCst),
        3,
        "resolver.resolve() called each attempt"
    );
}

/// Exercises the 401 abort path end-to-end via a mock axum server.
///
/// On the first 401, `upload_with_retries` re-resolves credentials and
/// retries once. If the second attempt also returns 401, it aborts.
#[tokio::test]
async fn upload_with_retries_aborts_on_persistent_auth_error() {
    use axum::{
        Router, body::Body, extract::State, http::StatusCode, response::IntoResponse, routing::post,
    };

    #[derive(Clone)]
    struct TestState {
        request_count: Arc<AtomicU32>,
    }

    async fn handler_401(State(s): State<TestState>, _body: Body) -> impl IntoResponse {
        s.request_count.fetch_add(1, Ordering::SeqCst);
        (StatusCode::UNAUTHORIZED, "Invalid token")
    }

    let state = TestState {
        request_count: Arc::new(AtomicU32::new(0)),
    };

    let app = Router::new()
        .route("/v1/storage", post(handler_401))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let resolve_count = Arc::new(AtomicU32::new(0));
    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: resolve_count.clone(),
        proxy_base_url: format!("http://{}/v1", addr),
    });

    let policy = UploadRetryPolicy {
        max_attempts: 5,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        multiplier: 1.0,
        max_age: DEFAULT_MAX_AGE,
        auth_park_probe_interval: DEFAULT_AUTH_PARK_PROBE_INTERVAL,
    };

    let temp = tempfile::TempDir::new().unwrap();
    let file_path = temp.path().join("test.json");
    std::fs::write(&file_path, b"test data").unwrap();

    let mut item = UploadQueueItem {
        source: UploadSource::OwnedTemp(file_path),
        gcs_path: "session/turn_0/test.json".to_string(),
        content_type: "application/json".to_string(),
        artifact_name: "test".to_string(),
        attempts: 0,
        enqueued_at: Instant::now(),
        sidecar_path: None,
        completion_tx: None,
        client_version: None,
        compress: false,
        parent_span: tracing::Span::none(),
        _in_flight: None,
    };

    let result = run_upload_with_retries(&mut item, &resolver, &policy).await;
    assert!(result.is_err());
    let err_msg = format!("{:#}", result.unwrap_err());
    assert!(
        err_msg.contains("401"),
        "error should mention 401: {}",
        err_msg
    );
    // First attempt gets 401, retries once with fresh creds, second attempt
    // also 401 → aborts. So 2 attempts, 2 resolves, 2 HTTP requests.
    assert_eq!(
        item.attempts, 2,
        "should retry once after auth error then abort"
    );
    assert_eq!(
        resolve_count.load(Ordering::SeqCst),
        2,
        "credentials re-resolved once for the auth retry"
    );
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        2,
        "two HTTP requests: initial + one auth retry"
    );
}

// ------------------------------------------------------------------
// Park-on-401 tests
// ------------------------------------------------------------------

/// Ignores the worker's `timeout` in favor of the short `wait_slice` —
/// early-returning waits are tolerated by the park loop, and tests
/// shouldn't sit through the production 5s interval.
struct ParkingResolver {
    proxy_base_url: String,
    token_gen: tokio::sync::watch::Sender<u64>,
    hook_enabled: bool,
    wait_slice: Duration,
    seen_bearers: Mutex<Vec<Option<String>>>,
    usable: std::sync::atomic::AtomicBool,
}

impl ParkingResolver {
    fn new(proxy_base_url: String) -> Self {
        Self {
            proxy_base_url,
            token_gen: tokio::sync::watch::channel(0).0,
            hook_enabled: true,
            wait_slice: Duration::from_millis(10),
            seen_bearers: Mutex::new(Vec::new()),
            usable: std::sync::atomic::AtomicBool::new(true),
        }
    }

    fn signal_recovery(&self) {
        self.token_gen.send_modify(|g| *g += 1);
    }

    fn set_usable(&self, v: bool) {
        self.usable.store(v, Ordering::SeqCst);
    }
}

impl TraceExportSource for ParkingResolver {
    fn has_usable_credential(&self) -> bool {
        self.usable.load(Ordering::SeqCst)
    }

    fn resolve(&self) -> TraceExportConfig {
        TraceExportConfig {
            bucket_url: None,
            service_account_key: None,
            prefix_dir: None,
            gcs_prefix: None,
            absolute_paths: false,
            archive_name_override: None,
            upload_method: UploadMethod::Proxy {
                proxy_base_url: self.proxy_base_url.clone(),
                user_token: "test-token".to_string(),
                deployment_key: None,
                alpha_test_key: None,
            },
        }
    }

    fn wait_for_auth_recovery(
        &self,
        failed_bearer: Option<&str>,
        _timeout: Duration,
    ) -> Option<std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + '_>>> {
        self.seen_bearers
            .lock()
            .unwrap()
            .push(failed_bearer.map(str::to_owned));
        if !self.hook_enabled {
            return None;
        }
        let mut rx = self.token_gen.subscribe();
        let slice = self.wait_slice;
        Some(Box::pin(async move {
            // Level-triggered: the park loop rebuilds this future every
            // slice, so a recovery signal that already fired before this
            // subscribe must be observed from the current value rather than
            // waited for as a future edge (which would be lost).
            if *rx.borrow() > 0 {
                return true;
            }
            tokio::select! {
                r = rx.changed() => r.is_ok(),
                _ = tokio::time::sleep(slice) => false,
            }
        }))
    }
}

#[derive(Clone)]
struct FlippableAuthState {
    request_count: Arc<AtomicU32>,
    unauthorized: Arc<std::sync::atomic::AtomicBool>,
}

async fn flippable_auth_handler(
    axum::extract::State(s): axum::extract::State<FlippableAuthState>,
    _body: axum::body::Body,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    s.request_count.fetch_add(1, Ordering::SeqCst);
    if s.unauthorized.load(Ordering::SeqCst) {
        return (axum::http::StatusCode::UNAUTHORIZED, "Invalid token").into_response();
    }
    (
        axum::http::StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        r#"{"bucket":"b","path":"p","size":1,"content_type":"application/json","generation":1}"#,
    )
        .into_response()
}

async fn spawn_flippable_server(initially_unauthorized: bool) -> (FlippableAuthState, String) {
    use axum::{Router, routing::post};
    let state = FlippableAuthState {
        request_count: Arc::new(AtomicU32::new(0)),
        unauthorized: Arc::new(std::sync::atomic::AtomicBool::new(initially_unauthorized)),
    };
    let app = Router::new()
        .route("/v1/storage", post(flippable_auth_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (state, format!("http://{}/v1", addr))
}

fn park_test_item(
    temp: &tempfile::TempDir,
) -> (
    UploadQueueItem,
    oneshot::Receiver<anyhow::Result<UploadCompletion>>,
) {
    let file_path = temp.path().join("test.json");
    std::fs::write(&file_path, b"test data").unwrap();
    let (tx, rx) = oneshot::channel();
    (
        UploadQueueItem {
            source: UploadSource::OwnedTemp(file_path),
            gcs_path: "session/turn_0/test.json".to_string(),
            content_type: "application/json".to_string(),
            artifact_name: "test".to_string(),
            attempts: 0,
            enqueued_at: Instant::now(),
            sidecar_path: None,
            completion_tx: Some(tx),
            client_version: None,
            compress: false,
            parent_span: tracing::Span::none(),
            _in_flight: None,
        },
        rx,
    )
}

/// The pre-park behavior dropped the artifact at this exact point.
#[tokio::test]
async fn parked_item_uploads_after_auth_recovery() {
    let (state, url) = spawn_flippable_server(true).await;
    let resolver = Arc::new(ParkingResolver::new(url));
    let resolver_dyn: Arc<dyn TraceExportSource> = resolver.clone();
    let stats = Arc::new(UploadQueueStats::new());
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let temp = tempfile::TempDir::new().unwrap();
    let (mut item, completion_rx) = park_test_item(&temp);

    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        ..Default::default()
    };

    let task = {
        let resolver = resolver_dyn.clone();
        let stats = stats.clone();
        let draining = draining.clone();
        tokio::spawn(async move {
            upload_with_retries(&mut item, &resolver, &policy, 100, &stats, &draining, None).await
        })
    };

    // Latency contract: callers never wait on auth recovery.
    let parked_err = tokio::time::timeout(Duration::from_secs(5), completion_rx)
        .await
        .expect("waiter released before recovery")
        .expect("completion channel alive");
    let msg = format!(
        "{:#}",
        parked_err.expect_err("parked notification is an Err")
    );
    assert!(
        msg.contains("parked"),
        "waiter sees the parked marker: {msg}"
    );
    assert_eq!(stats.auth_parked.load(Ordering::Relaxed), 1);
    let requests_while_parked = state.request_count.load(Ordering::SeqCst);
    assert_eq!(
        requests_while_parked, 2,
        "initial attempt + one refresh retry"
    );

    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        requests_while_parked,
        "no probe traffic while parked"
    );

    state.unauthorized.store(false, Ordering::SeqCst);
    resolver.signal_recovery();

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("parked item resumes after recovery")
        .expect("task join");
    assert!(result.is_ok(), "upload succeeds after recovery: {result:?}");
    assert_eq!(state.request_count.load(Ordering::SeqCst), 3);
    assert_eq!(
        resolver.seen_bearers.lock().unwrap().first(),
        Some(&Some("test-token".to_owned())),
        "hook receives the bearer the rejected attempt used"
    );
}

/// Recovery detection must be level-triggered: the park loop rebuilds its
/// wait future every slice, so a `signal_recovery()` that lands in the gap
/// between one slice finishing and the next subscribe must still be seen.
/// An edge-triggered watch loses that signal, leaving the item parked for a
/// full `auth_park_probe_interval` (300s) and timing out the resume wait.
#[tokio::test]
async fn parking_resolver_recovery_is_level_triggered() {
    let (_state, url) = spawn_flippable_server(true).await;
    let resolver = ParkingResolver::new(url);
    // Recovery fires *before* the next wait future subscribes (the race the
    // park loop hits between slices).
    resolver.signal_recovery();
    let wait = resolver
        .wait_for_auth_recovery(Some("test-token"), AUTH_PARK_WAIT_INTERVAL)
        .expect("hook enabled");
    assert!(
        wait.await,
        "recovery signaled before subscribe must still wake the parked item"
    );
}

/// A parked item releases its concurrency permit (parking does zero wire
/// I/O) so other uploads keep flowing during an auth outage, then
/// re-acquires it before resuming. Without release, `max_concurrent` parked
/// items would pin every worker slot for up to `max_age` and stall
/// dispatch/drain.
#[tokio::test]
async fn parked_item_releases_concurrency_permit() {
    let (state, url) = spawn_flippable_server(true).await;
    let resolver = Arc::new(ParkingResolver::new(url));
    let resolver_dyn: Arc<dyn TraceExportSource> = resolver.clone();
    let stats = Arc::new(UploadQueueStats::new());
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let temp = tempfile::TempDir::new().unwrap();
    let (mut item, _completion_rx) = park_test_item(&temp);

    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        ..Default::default()
    };

    // A single slot: if parking kept its permit, the slot would stay
    // pinned at zero for the whole park.
    let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
    let held = semaphore.clone().acquire_owned().await.unwrap();
    assert_eq!(semaphore.available_permits(), 0);
    let mut concurrency = ConcurrencyPermit {
        semaphore: semaphore.clone(),
        permit: Some(held),
    };

    let task = {
        let resolver = resolver_dyn.clone();
        let stats = stats.clone();
        let draining = draining.clone();
        tokio::spawn(async move {
            let r = upload_with_retries(
                &mut item,
                &resolver,
                &policy,
                100,
                &stats,
                &draining,
                Some(&mut concurrency),
            )
            .await;
            // On a successful resume the slot is held again.
            (r, concurrency.permit.is_some())
        })
    };

    // The slot frees up while the item is parked (mid-flight, not done).
    tokio::time::timeout(Duration::from_secs(2), async {
        while semaphore.available_permits() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("parked item releases its concurrency permit");
    assert_eq!(stats.auth_parked.load(Ordering::Relaxed), 1);

    // Heal + wake → the item re-acquires the slot and finishes.
    state.unauthorized.store(false, Ordering::SeqCst);
    resolver.signal_recovery();

    let (result, held_after) = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("parked item resumes after recovery")
        .expect("task join");
    assert!(result.is_ok(), "upload succeeds after recovery: {result:?}");
    assert!(
        held_after,
        "permit re-acquired before the post-park wire attempt"
    );
}

/// Without a recovery hook the item is dropped, never parked: the waiter
/// must receive the original 401 error, not the parked marker.
#[tokio::test]
async fn no_hook_drops_without_park_marker() {
    let (_state, url) = spawn_flippable_server(true).await;
    let mut resolver = ParkingResolver::new(url);
    resolver.hook_enabled = false;
    let resolver: Arc<dyn TraceExportSource> = Arc::new(resolver);
    let stats = Arc::new(UploadQueueStats::new());
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let temp = tempfile::TempDir::new().unwrap();
    let (mut item, completion_rx) = park_test_item(&temp);

    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        ..Default::default()
    };

    let result =
        upload_with_retries(&mut item, &resolver, &policy, 100, &stats, &draining, None).await;
    assert!(result.is_err());
    assert_eq!(
        stats.auth_parked.load(Ordering::Relaxed),
        0,
        "no park entry without a recovery hook"
    );
    // The waiter was never notified inside upload_with_retries; the tx is
    // intact for process_item's terminal notification (legacy contract).
    assert!(
        item.completion_tx.is_some(),
        "completion stays with the caller's terminal error path"
    );
    drop(item);
    let waiter = completion_rx.await;
    assert!(
        waiter.is_err(),
        "oneshot closes without a parked notification"
    );
}

/// Draining and a recovery wake racing: the wake must re-run the guards
/// and never reach the wire once draining is set.
#[tokio::test]
async fn parked_wake_revalidates_drain_before_wire() {
    let (state, url) = spawn_flippable_server(true).await;
    let resolver = Arc::new(ParkingResolver::new(url));
    let resolver_dyn: Arc<dyn TraceExportSource> = resolver.clone();
    let stats = Arc::new(UploadQueueStats::new());
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let temp = tempfile::TempDir::new().unwrap();
    let (mut item, _completion_rx) = park_test_item(&temp);

    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        ..Default::default()
    };

    let task = {
        let resolver = resolver_dyn.clone();
        let stats = stats.clone();
        let draining = draining.clone();
        tokio::spawn(async move {
            upload_with_retries(&mut item, &resolver, &policy, 100, &stats, &draining, None).await
        })
    };

    while stats.auth_parked.load(Ordering::Relaxed) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Heal the server so a wire attempt WOULD succeed, then set draining
    // before signaling the wake: drain must win.
    state.unauthorized.store(false, Ordering::SeqCst);
    draining.store(true, Ordering::Relaxed);
    resolver.signal_recovery();

    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("bails out promptly")
        .expect("task join");
    assert!(result.is_err(), "drain wins over a pending wake");
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        2,
        "no wire attempt after draining is set"
    );
}

/// With a recovery hook that never fires, the probe interval still
/// retries: a server-side 401 blip heals without a client token change.
#[tokio::test]
async fn parked_item_probe_retries_without_token_change() {
    let (state, url) = spawn_flippable_server(true).await;
    let resolver: Arc<dyn TraceExportSource> = Arc::new(ParkingResolver::new(url));
    let stats = Arc::new(UploadQueueStats::new());
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let temp = tempfile::TempDir::new().unwrap();
    let (mut item, _completion_rx) = park_test_item(&temp);

    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        auth_park_probe_interval: Duration::from_millis(50),
        ..Default::default()
    };

    // Heal only once the item has actually parked — a wall-clock delay
    // races slow runners where the refresh retry itself lands after the
    // heal and never parks.
    {
        let state = state.clone();
        let stats = stats.clone();
        tokio::spawn(async move {
            while stats.auth_parked.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            state.unauthorized.store(false, Ordering::SeqCst);
        });
    }

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        upload_with_retries(&mut item, &resolver, &policy, 100, &stats, &draining, None),
    )
    .await
    .expect("probe path resumes the upload");
    assert!(result.is_ok(), "upload succeeds via probe: {result:?}");
    assert_eq!(stats.auth_parked.load(Ordering::Relaxed), 1);
    assert!(
        state.request_count.load(Ordering::SeqCst) >= 3,
        "initial + refresh retry + at least one probe"
    );
}

#[tokio::test]
async fn parked_item_skips_probe_without_usable_credential() {
    let (state, url) = spawn_flippable_server(true).await;
    let resolver = Arc::new(ParkingResolver::new(url));
    resolver.set_usable(false);
    let resolver_dyn: Arc<dyn TraceExportSource> = resolver.clone();
    let stats = Arc::new(UploadQueueStats::new());
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let temp = tempfile::TempDir::new().unwrap();
    let (mut item, _completion_rx) = park_test_item(&temp);

    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        auth_park_probe_interval: Duration::from_millis(20),
        ..Default::default()
    };

    {
        let state = state.clone();
        let stats = stats.clone();
        let resolver = resolver.clone();
        tokio::spawn(async move {
            while stats.auth_parked.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let at_park = state.request_count.load(Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(150)).await;
            assert_eq!(
                state.request_count.load(Ordering::SeqCst),
                at_park,
                "no blind wire probe while the credential is unusable",
            );
            state.unauthorized.store(false, Ordering::SeqCst);
            resolver.set_usable(true);
            resolver.signal_recovery();
        });
    }

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        upload_with_retries(
            &mut item,
            &resolver_dyn,
            &policy,
            100,
            &stats,
            &draining,
            None,
        ),
    )
    .await
    .expect("upload resumes once the credential is usable");
    assert!(
        result.is_ok(),
        "upload succeeds after creds recover: {result:?}"
    );
    assert_eq!(stats.auth_parked.load(Ordering::Relaxed), 1);
}

/// Draining flips while an item is parked → the item bails out promptly
/// (legacy drop) instead of holding `drain()` until its timeout.
#[tokio::test]
async fn parked_item_bails_out_on_drain() {
    let (state, url) = spawn_flippable_server(true).await;
    let resolver: Arc<dyn TraceExportSource> = Arc::new(ParkingResolver::new(url));
    let stats = Arc::new(UploadQueueStats::new());
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let temp = tempfile::TempDir::new().unwrap();
    let (mut item, _completion_rx) = park_test_item(&temp);

    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        ..Default::default()
    };

    let task = {
        let resolver = resolver.clone();
        let stats = stats.clone();
        let draining = draining.clone();
        tokio::spawn(async move {
            upload_with_retries(&mut item, &resolver, &policy, 100, &stats, &draining, None).await
        })
    };

    while stats.auth_parked.load(Ordering::Relaxed) == 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    draining.store(true, Ordering::Relaxed);

    let result = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("parked item bails out promptly when draining")
        .expect("task join");
    assert!(result.is_err(), "drain bail-out is a failure outcome");
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        2,
        "no extra wire attempts on drain bail-out"
    );
}

/// A parked item that outlives `max_age` is dropped (disk bound holds).
#[tokio::test]
async fn parked_item_expires_at_max_age() {
    let (_state, url) = spawn_flippable_server(true).await;
    let resolver: Arc<dyn TraceExportSource> = Arc::new(ParkingResolver::new(url));
    let stats = Arc::new(UploadQueueStats::new());
    let draining = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let temp = tempfile::TempDir::new().unwrap();
    let (mut item, _completion_rx) = park_test_item(&temp);

    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        max_age: Duration::from_millis(150),
        ..Default::default()
    };

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        upload_with_retries(&mut item, &resolver, &policy, 100, &stats, &draining, None),
    )
    .await
    .expect("expires instead of parking forever");
    assert!(result.is_err(), "max_age bound enforced while parked");
    assert_eq!(stats.auth_parked.load(Ordering::Relaxed), 1);
}

/// A terminal status (400/403/404, 525/526) must abort on the FIRST attempt:
/// one HTTP request, one credential resolve, no backoff.
async fn assert_terminal_status_aborts_immediately(status: axum::http::StatusCode) {
    use axum::{Router, body::Body, extract::State, response::IntoResponse, routing::post};

    #[derive(Clone)]
    struct TestState {
        request_count: Arc<AtomicU32>,
        status: axum::http::StatusCode,
    }

    async fn handler(State(s): State<TestState>, _body: Body) -> impl IntoResponse {
        s.request_count.fetch_add(1, Ordering::SeqCst);
        (s.status, "terminal")
    }

    let state = TestState {
        request_count: Arc::new(AtomicU32::new(0)),
        status,
    };
    let app = Router::new()
        .route("/v1/storage", post(handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let resolve_count = Arc::new(AtomicU32::new(0));
    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: resolve_count.clone(),
        proxy_base_url: format!("http://{}/v1", addr),
    });
    let policy = UploadRetryPolicy {
        max_attempts: 5,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        multiplier: 1.0,
        max_age: DEFAULT_MAX_AGE,
        auth_park_probe_interval: DEFAULT_AUTH_PARK_PROBE_INTERVAL,
    };
    let temp = tempfile::TempDir::new().unwrap();
    let file_path = temp.path().join("test.json");
    std::fs::write(&file_path, b"test data").unwrap();
    let mut item = UploadQueueItem {
        source: UploadSource::OwnedTemp(file_path),
        gcs_path: "session/turn_0/test.json".to_string(),
        content_type: "application/json".to_string(),
        artifact_name: "test".to_string(),
        attempts: 0,
        enqueued_at: Instant::now(),
        sidecar_path: None,
        completion_tx: None,
        client_version: None,
        compress: false,
        parent_span: tracing::Span::none(),
        _in_flight: None,
    };

    let result = run_upload_with_retries(&mut item, &resolver, &policy).await;
    assert!(result.is_err(), "terminal {status} must fail");
    assert_eq!(
        item.attempts, 1,
        "terminal {status} must abort on the first attempt with no retries"
    );
    assert_eq!(
        resolve_count.load(Ordering::SeqCst),
        1,
        "credentials resolved exactly once (no retry) for terminal {status}"
    );
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        1,
        "exactly one HTTP request — no retry budget burned on terminal {status}"
    );
}

#[tokio::test]
async fn upload_with_retries_aborts_immediately_on_404() {
    // not_owner — the ownership gate's opaque 404.
    assert_terminal_status_aborts_immediately(axum::http::StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn upload_with_retries_aborts_immediately_on_400() {
    // bad_path — the gate's 400 for a structurally-invalid path.
    assert_terminal_status_aborts_immediately(axum::http::StatusCode::BAD_REQUEST).await;
}

#[tokio::test]
async fn upload_with_retries_aborts_immediately_on_403() {
    // ZDR team — storage upload rejected deterministically.
    assert_terminal_status_aborts_immediately(axum::http::StatusCode::FORBIDDEN).await;
}

/// 401 on first attempt, then success on retry with fresh credentials.
#[tokio::test]
async fn upload_with_retries_recovers_after_auth_refresh() {
    use axum::{
        Router, body::Body, extract::State, http::StatusCode, response::IntoResponse, routing::post,
    };

    #[derive(Clone)]
    struct TestState {
        request_count: Arc<AtomicU32>,
    }

    async fn handler_401_then_ok(State(s): State<TestState>, _body: Body) -> impl IntoResponse {
        let n = s.request_count.fetch_add(1, Ordering::SeqCst);
        if n == 0 {
            (StatusCode::UNAUTHORIZED, "Invalid token").into_response()
        } else {
            let body = r#"{"bucket":"b","path":"p","size":9,"content_type":"application/json","generation":1}"#;
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
    }

    let state = TestState {
        request_count: Arc::new(AtomicU32::new(0)),
    };

    let app = Router::new()
        .route("/v1/storage", post(handler_401_then_ok))
        .with_state(state.clone());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let resolve_count = Arc::new(AtomicU32::new(0));
    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: resolve_count.clone(),
        proxy_base_url: format!("http://{}/v1", addr),
    });

    let policy = UploadRetryPolicy {
        max_attempts: 5,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        multiplier: 1.0,
        max_age: DEFAULT_MAX_AGE,
        auth_park_probe_interval: DEFAULT_AUTH_PARK_PROBE_INTERVAL,
    };

    let temp = tempfile::TempDir::new().unwrap();
    let file_path = temp.path().join("test.json");
    std::fs::write(&file_path, b"test data").unwrap();

    let mut item = UploadQueueItem {
        source: UploadSource::OwnedTemp(file_path),
        gcs_path: "session/turn_0/test.json".to_string(),
        content_type: "application/json".to_string(),
        artifact_name: "test".to_string(),
        attempts: 0,
        enqueued_at: Instant::now(),
        sidecar_path: None,
        completion_tx: None,
        client_version: None,
        compress: false,
        parent_span: tracing::Span::none(),
        _in_flight: None,
    };

    let result = run_upload_with_retries(&mut item, &resolver, &policy).await;
    assert!(result.is_ok(), "should succeed after auth refresh");
    assert_eq!(item.attempts, 2, "first attempt 401, second attempt OK");
    assert_eq!(
        resolve_count.load(Ordering::SeqCst),
        2,
        "credentials resolved twice"
    );
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        2,
        "two HTTP requests total"
    );
}

#[tokio::test]
async fn drain_no_pending_returns_zero() {
    let temp = tempfile::TempDir::new().unwrap();
    let resolver: Arc<dyn TraceExportSource> = Arc::new(MockResolver);
    let queue = UploadQueue::spawn(temp.path(), resolver, UploadRetryPolicy::default());
    let result = queue.drain(Duration::from_secs(1)).await;
    assert_eq!(result, 0);
}

#[tokio::test]
async fn double_drain_is_noop() {
    let temp = tempfile::TempDir::new().unwrap();
    let resolver: Arc<dyn TraceExportSource> = Arc::new(MockResolver);
    let queue = UploadQueue::spawn(temp.path(), resolver, UploadRetryPolicy::default());
    assert_eq!(queue.drain(Duration::from_secs(1)).await, 0);
    assert_eq!(queue.drain(Duration::from_secs(1)).await, 0);
}

#[tokio::test]
async fn enqueue_after_drain_falls_back_to_inline() {
    let temp = tempfile::TempDir::new().unwrap();
    let resolver: Arc<dyn TraceExportSource> = Arc::new(MockResolver);
    let queue = UploadQueue::spawn(temp.path(), resolver, UploadRetryPolicy::default());

    queue.drain(Duration::from_secs(1)).await;

    let before = queue.stats().enqueue_fallbacks.load(Ordering::Relaxed);
    queue
        .enqueue(b"data", "test/path", "text/plain", "test", "sess", 0)
        .await
        .unwrap();
    let after = queue.stats().enqueue_fallbacks.load(Ordering::Relaxed);
    assert!(
        after > before,
        "enqueue after drain should fall back to inline upload"
    );
}

async fn spawn_test_server(app: axum::Router) -> Arc<dyn TraceExportSource> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Arc::new(CountingResolver {
        count: Arc::new(AtomicU32::new(0)),
        proxy_base_url: format!("http://{}/v1", addr),
    })
}

#[tokio::test]
async fn drain_processes_pending_items() {
    use axum::{Router, body::Body, http::StatusCode, response::IntoResponse, routing::post};

    async fn ok_handler(_body: Body) -> impl IntoResponse {
        let body =
            r#"{"bucket":"b","path":"p","size":4,"content_type":"text/plain","generation":1}"#;
        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
    }

    let app = Router::new().route("/v1/storage", post(ok_handler));
    let resolver = spawn_test_server(app).await;

    let temp = tempfile::TempDir::new().unwrap();
    let queue = UploadQueue::spawn(temp.path(), resolver, UploadRetryPolicy::default());

    queue
        .enqueue(
            b"payload",
            "session/turn_0/test.json",
            "application/json",
            "test",
            "sess-drain",
            0,
        )
        .await
        .unwrap();

    let result = queue.drain(Duration::from_secs(5)).await;
    assert_eq!(result, 0, "all items should be processed during drain");
    assert_eq!(
        queue.stats().uploaded.load(Ordering::Relaxed),
        1,
        "one item should have been uploaded"
    );
}

/// A full enqueue→process cycle settles `inflight` and `pending` to zero
/// and pings the wired transition listener.
#[tokio::test]
async fn drain_settles_inflight_and_pending_to_zero() {
    use axum::{Router, body::Body, http::StatusCode, response::IntoResponse, routing::post};

    async fn ok_handler(_body: Body) -> impl IntoResponse {
        let body =
            r#"{"bucket":"b","path":"p","size":4,"content_type":"text/plain","generation":1}"#;
        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body,
        )
    }

    let app = Router::new().route("/v1/storage", post(ok_handler));
    let resolver = spawn_test_server(app).await;
    let temp = tempfile::TempDir::new().unwrap();
    let queue = UploadQueue::spawn(temp.path(), resolver, UploadRetryPolicy::default());

    let notify = Arc::new(Notify::new());
    queue.stats().set_transition_notify(notify.clone());
    let pings = Arc::new(AtomicU64::new(0));
    let pings_task = {
        let pings = pings.clone();
        let notify = notify.clone();
        tokio::spawn(async move {
            loop {
                notify.notified().await;
                pings.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    tokio::task::yield_now().await;

    queue
        .enqueue(
            b"payload",
            "session/turn_0/test.json",
            "application/json",
            "test",
            "sess-inflight",
            0,
        )
        .await
        .unwrap();

    let result = queue.drain(Duration::from_secs(5)).await;
    assert_eq!(result, 0, "item processed during drain");

    let stats = queue.stats();
    assert_eq!(
        stats.inflight.load(Ordering::Relaxed),
        0,
        "inflight must settle back to zero after the upload completes"
    );
    assert_eq!(
        stats.pending.load(Ordering::Relaxed),
        0,
        "pending must settle back to zero"
    );
    assert_eq!(stats.uploaded.load(Ordering::Relaxed), 1);
    assert!(
        pings.load(Ordering::SeqCst) > 0,
        "the wired transition listener must have been pinged across enqueue/complete"
    );
    pings_task.abort();
}

#[tokio::test]
async fn drain_timeout_returns_pending_count() {
    use axum::{Router, body::Body, http::StatusCode, response::IntoResponse, routing::post};

    async fn slow_handler(_body: Body) -> impl IntoResponse {
        tokio::time::sleep(Duration::from_secs(60)).await;
        (StatusCode::OK, "ok")
    }

    let app = Router::new().route("/v1/storage", post(slow_handler));
    let resolver = spawn_test_server(app).await;

    let temp = tempfile::TempDir::new().unwrap();
    let policy = UploadRetryPolicy {
        max_attempts: 1,
        ..Default::default()
    };
    let queue = UploadQueue::spawn_with_concurrency(temp.path(), resolver, policy, 1);

    queue
        .enqueue(
            b"payload",
            "session/turn_0/slow.json",
            "application/json",
            "slow",
            "sess-timeout",
            0,
        )
        .await
        .unwrap();

    // Test is race-free: if the worker picks up the item before drain, it's
    // stuck in the 60s handler; if still in channel, the drain loop dispatches
    // it to the same slow handler. Either way the 100ms deadline expires.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let result = queue.drain(Duration::from_millis(100)).await;
    assert!(result > 0, "should have pending items after timeout");
}

/// A parked item releases its semaphore permit, so the worker's drain must
/// wait on the spawned task (not just permit availability) — otherwise it
/// reports completion while the parked upload is still running and `pending`
/// is still nonzero.
#[tokio::test]
async fn drain_waits_for_parked_task_to_bail() {
    let (_state, url) = spawn_flippable_server(true).await; // 401 from the start
    let resolver: Arc<dyn TraceExportSource> = Arc::new(ParkingResolver::new(url));

    let temp = tempfile::TempDir::new().unwrap();
    let policy = UploadRetryPolicy {
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        ..Default::default()
    };
    let queue = UploadQueue::spawn_with_concurrency(temp.path(), resolver, policy, 1);

    queue
        .enqueue(
            b"payload",
            "session/turn_0/park.json",
            "application/json",
            "park",
            "sess-park-drain",
            0,
        )
        .await
        .unwrap();

    // Wait until the item has parked (and thus released its only permit).
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while queue.stats().auth_parked.load(Ordering::Relaxed) == 0 {
        assert!(std::time::Instant::now() < deadline, "item never parked");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    let result = queue.drain(Duration::from_secs(5)).await;
    assert_eq!(result, 0, "drain completes after the parked task bails");
    assert_eq!(
        queue.stats().pending.load(Ordering::Relaxed),
        0,
        "drain waited for the parked task to finish before returning"
    );
}

#[test]
fn cleanup_orphaned_uploads_stores_count_in_static() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    // Create two stale files with mtime set to 3 hours ago.
    let three_hours_ago = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
    let ft = filetime::FileTime::from_system_time(three_hours_ago);
    for name in ["stale_a.json", "stale_b.json"] {
        let path = queue_dir.join(name);
        std::fs::write(&path, b"old").unwrap();
        filetime::set_file_mtime(&path, ft).unwrap();
    }

    // Create a fresh file that should survive.
    std::fs::write(queue_dir.join("fresh.json"), b"new").unwrap();

    let cleaned = cleanup_orphaned_uploads(temp.path(), Duration::from_secs(3600));
    assert_eq!(cleaned, 2, "should report 2 stale files removed");
    assert_eq!(
        last_orphans_cleaned(),
        2,
        "static should match the returned count"
    );

    // Fresh file survives.
    assert!(queue_dir.join("fresh.json").exists());
}

/// The byte-budget permit math: 1 MiB units rounded up, floor of 1, and a
/// hard clamp to the semaphore's total so an oversized file never requests
/// more permits than exist (which would deadlock `acquire_many` / overflow
/// `u32`).
#[test]
fn inline_fallback_permits_clamps_and_never_overflows() {
    // Floor of 1: even a zero-byte upload takes one permit.
    assert_eq!(inline_fallback_permits(0), 1);
    assert_eq!(inline_fallback_permits(1), 1);
    assert_eq!(inline_fallback_permits(INLINE_FALLBACK_PERMIT_BYTES), 1);
    assert_eq!(inline_fallback_permits(INLINE_FALLBACK_PERMIT_BYTES + 1), 2);
    assert_eq!(inline_fallback_permits(2 * INLINE_FALLBACK_PERMIT_BYTES), 2);

    // Exact clamp boundary: the whole budget maps to TOTAL naturally (no
    // clamp), and one byte over still yields TOTAL (clamped down by one).
    assert_eq!(
        inline_fallback_permits(MAX_INLINE_FALLBACK_INFLIGHT_BYTES),
        INLINE_FALLBACK_TOTAL_PERMITS
    );
    assert_eq!(
        inline_fallback_permits(MAX_INLINE_FALLBACK_INFLIGHT_BYTES + 1),
        INLINE_FALLBACK_TOTAL_PERMITS
    );

    let huge = 8u64 * 1024 * 1024 * 1024; // 8 GiB
    let permits = inline_fallback_permits(huge);
    assert_eq!(permits, INLINE_FALLBACK_TOTAL_PERMITS);

    // u64::MAX must not overflow u32 nor exceed the semaphore capacity.
    let permits_max = inline_fallback_permits(u64::MAX);
    assert_eq!(permits_max, INLINE_FALLBACK_TOTAL_PERMITS);

    // The clamped request is acquirable from a full-size semaphore: no
    // panic, no deadlock, no "more permits than exist" error.
    let sem = tokio::sync::Semaphore::new(INLINE_FALLBACK_TOTAL_PERMITS as usize);
    let acquired = sem.try_acquire_many(permits);
    assert!(
        acquired.is_ok(),
        "clamped permits must be acquirable from the semaphore"
    );
}

/// The over-budget `enqueue_file` fallback streams the source file **at
/// upload time**, not at enqueue time. This would FAIL against a slurp
/// implementation (`std::fs::read` at enqueue): we hold the upload parked on
/// the (0-permit) semaphore, overwrite the source with *different* bytes
/// after `enqueue_file` returns, then release the permit and assert the
/// backend received the **new** bytes — proving the read happened at upload
/// time from the path, not eagerly into memory. Also checks `enqueue_fallbacks`
/// bumps, no temp copy is staged, the source is preserved, and `pending_bytes`
/// is untouched.
#[tokio::test]
async fn enqueue_file_over_budget_streams_source_at_upload_time() {
    use axum::{
        Router, body::Bytes, extract::State, http::StatusCode, response::IntoResponse,
        routing::post,
    };

    #[derive(Clone)]
    struct TestState {
        request_count: Arc<AtomicU32>,
        last_body: Arc<Mutex<Vec<u8>>>,
    }

    async fn ok_handler(State(s): State<TestState>, body: Bytes) -> impl IntoResponse {
        *s.last_body.lock().unwrap() = body.to_vec();
        s.request_count.fetch_add(1, Ordering::SeqCst);
        let resp = r#"{"bucket":"b","path":"p","size":1,"content_type":"application/octet-stream","generation":1}"#;
        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            resp,
        )
    }

    let state = TestState {
        request_count: Arc::new(AtomicU32::new(0)),
        last_body: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/v1/storage", post(ok_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: Arc::new(AtomicU32::new(0)),
        proxy_base_url: format!("http://{}/v1", addr),
    });

    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let source = temp.path().join("image.bin");
    let original = vec![0xAAu8; 4096];
    std::fs::write(&source, &original).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    // Force over-budget: pending_bytes already at the cap so any add trips it.
    let max_queue_bytes: u64 = 1000;
    stats
        .pending_bytes
        .store(max_queue_bytes, Ordering::Relaxed);
    let pre_pending = stats.pending_bytes.load(Ordering::Relaxed);

    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    // Semaphore starts at 0 permits: the spawned upload task parks on
    // `acquire_many_owned` BEFORE `upload_file` opens the source. A slurp
    // implementation would have already captured the original bytes at
    // enqueue time, so the discriminating assertion below would fail for it.
    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver,
        stats: stats.clone(),
        max_queue_bytes,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(0)),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    queue
        .enqueue_file(
            &source,
            "session/turn_0/image.bin",
            "application/octet-stream",
            "image",
            "session-over",
            0,
        )
        .await
        .expect("over-budget enqueue_file must return Ok");

    assert_eq!(
        stats.enqueue_fallbacks.load(Ordering::Relaxed),
        1,
        "over-budget must bump enqueue_fallbacks exactly once"
    );
    // No copy is staged at all on the over-budget path (we stat the source).
    let staged: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert!(
        staged.is_empty(),
        "over-budget fallback must not stage a temp copy in queue_dir"
    );
    assert!(
        source.exists(),
        "source must remain on disk for path streaming"
    );
    assert_eq!(
        stats.pending_bytes.load(Ordering::Relaxed),
        pre_pending,
        "over-budget fallback must not bump pending_bytes"
    );
    // Nothing should have been uploaded yet — the task is parked on the 0-permit semaphore.
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        0,
        "upload must not run while the semaphore holds no permits"
    );

    // Mutate the source AFTER enqueue returns, BEFORE the upload runs.
    let updated = vec![0xBBu8; 4096];
    std::fs::write(&source, &updated).unwrap();

    // Release the parked task: now `upload_file` opens and streams the file.
    queue
        .inline_fallback_semaphore
        .add_permits(INLINE_FALLBACK_TOTAL_PERMITS as usize);

    for _ in 0..200 {
        if state.request_count.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        1,
        "inline fallback should stream-upload exactly once from the source path"
    );
    assert_eq!(
        *state.last_body.lock().unwrap(),
        updated,
        "backend must receive the UPDATED bytes (streamed at upload time); a \
         slurp at enqueue time would have sent the original bytes"
    );
}

/// Over budget AND the source is missing: `enqueue_file` returns `Err`
/// (the stat fails) instead of silently returning `Ok` and spawning a
/// streaming upload of a non-existent path. No fallback is counted.
#[tokio::test]
async fn enqueue_file_over_budget_missing_source_returns_err() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let missing = temp.path().join("does_not_exist.bin");

    let stats = Arc::new(UploadQueueStats::new());
    // Already over budget so the over-budget branch is taken.
    let max_queue_bytes: u64 = 1000;
    stats
        .pending_bytes
        .store(max_queue_bytes, Ordering::Relaxed);

    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = UploadQueue {
        tx,
        queue_dir,
        resolver: Arc::new(MockResolver),
        stats: stats.clone(),
        max_queue_bytes,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    let result = queue
        .enqueue_file(
            &missing,
            "session/turn_0/missing.bin",
            "application/octet-stream",
            "missing",
            "session-missing",
            0,
        )
        .await;

    assert!(
        result.is_err(),
        "missing source over budget must return Err, not silently Ok"
    );
    assert_eq!(
        stats.enqueue_fallbacks.load(Ordering::Relaxed),
        0,
        "no inline fallback should be spawned for a missing source"
    );
}

/// The `enqueue_file` channel-full / closed `try_send`-failure branch streams
/// from the source path and performs the decrement bookkeeping. Triggered by
/// dropping the receiver so `try_send` returns `Closed` (same fallback code
/// path as a full channel). Asserts `enqueue_fallbacks` bumps, `pending`/
/// `pending_bytes` are decremented back to zero, the source is preserved, and
/// the inline upload reaches the backend.
#[tokio::test]
async fn enqueue_file_channel_full_streams_from_source_path() {
    use axum::{
        Router, body::Body, extract::State, http::StatusCode, response::IntoResponse, routing::post,
    };

    #[derive(Clone)]
    struct TestState {
        request_count: Arc<AtomicU32>,
    }

    async fn ok_handler(State(s): State<TestState>, _body: Body) -> impl IntoResponse {
        s.request_count.fetch_add(1, Ordering::SeqCst);
        let resp = r#"{"bucket":"b","path":"p","size":1,"content_type":"application/octet-stream","generation":1}"#;
        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            resp,
        )
    }

    let state = TestState {
        request_count: Arc::new(AtomicU32::new(0)),
    };
    let app = Router::new()
        .route("/v1/storage", post(ok_handler))
        .with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: Arc::new(AtomicU32::new(0)),
        proxy_base_url: format!("http://{}/v1", addr),
    });

    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let source = temp.path().join("blob.bin");
    std::fs::write(&source, vec![0xCDu8; 2048]).unwrap();

    let stats = Arc::new(UploadQueueStats::new());

    // Drop the receiver so `try_send` fails with `Closed` → fallback branch.
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    drop(rx);
    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver,
        stats: stats.clone(),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES, // under budget: reaches try_send
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    queue
        .enqueue_file(
            &source,
            "session/turn_0/blob.bin",
            "application/octet-stream",
            "blob",
            "session-chanfull",
            0,
        )
        .await
        .expect("channel-full enqueue_file must return Ok");

    assert_eq!(
        stats.enqueue_fallbacks.load(Ordering::Relaxed),
        1,
        "channel-full must bump enqueue_fallbacks exactly once"
    );
    // The optimistic increments are rolled back by the fallback bookkeeping.
    assert_eq!(
        stats.pending.load(Ordering::Relaxed),
        0,
        "channel-full fallback must decrement pending back to zero"
    );
    assert_eq!(
        stats.pending_bytes.load(Ordering::Relaxed),
        0,
        "channel-full fallback must decrement pending_bytes back to zero"
    );
    // The rejected dest copy is removed; the source is streamed, not consumed.
    let staged: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert!(
        staged.is_empty(),
        "channel-full fallback must remove the rejected staged copy"
    );
    assert!(source.exists(), "source must remain on disk for streaming");

    for _ in 0..200 {
        if state.request_count.load(Ordering::SeqCst) >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        state.request_count.load(Ordering::SeqCst),
        1,
        "channel-full fallback should stream-upload exactly once from the source path"
    );
}

/// The byte-budget semaphore actually bounds inline-fallback concurrency:
/// firing more uploads than the permit budget allows, the observed peak
/// concurrency never exceeds the budget, and the excess tasks make progress
/// only after permits free up. Deterministic — uses a manual-reset gate
/// (`Semaphore::new(0)` + `add_permits`) and a counting "entered" semaphore
/// instead of sleeps. If the semaphore gating were deleted, peak concurrency
/// would equal the number of fired tasks and this test would fail.
///
/// This exercises `spawn_inline_upload_from_path`. The bytes helper
/// (`spawn_inline_upload`) and the blocking helper (`spawn_inline_upload_blocking`)
/// use the byte-identical `acquire_many_owned(inline_fallback_permits(..))` gating
/// idiom against the same shared semaphore, so the concurrency bound proven
/// here applies to all three; they are not separately parameterized.
#[tokio::test]
async fn inline_fallback_semaphore_bounds_concurrency() {
    use axum::{Router, body::Body, http::StatusCode, response::IntoResponse, routing::post};

    /// Resolver that parks each inline-upload task while it holds its permit,
    /// recording peak concurrency. It parks in `resolve_async` (after the
    /// permit is acquired, before `upload_file` opens the file) so the bound
    /// is observed before any real upload. After release it returns a config
    /// pointing at a fast mock server so the permit frees quickly and the
    /// next wave can run.
    struct ConcurrencyResolver {
        inflight: Arc<AtomicU32>,
        peak: Arc<AtomicU32>,
        started: Arc<AtomicU32>,
        /// add_permits(1) on entry; the test waits on this to count parked tasks.
        entered: Arc<tokio::sync::Semaphore>,
        /// starts at 0; the test releases tasks via add_permits.
        gate: Arc<tokio::sync::Semaphore>,
        proxy_base_url: String,
    }

    impl TraceExportSource for ConcurrencyResolver {
        fn resolve(&self) -> TraceExportConfig {
            TraceExportConfig {
                bucket_url: None,
                service_account_key: None,
                prefix_dir: None,
                gcs_prefix: None,
                absolute_paths: false,
                archive_name_override: None,
                upload_method: UploadMethod::Proxy {
                    proxy_base_url: self.proxy_base_url.clone(),
                    user_token: "t".to_string(),
                    deployment_key: None,
                    alpha_test_key: None,
                },
            }
        }

        fn resolve_async(
            &self,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = TraceExportConfig> + Send + '_>>
        {
            Box::pin(async move {
                let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
                self.peak.fetch_max(now, Ordering::SeqCst);
                self.started.fetch_add(1, Ordering::SeqCst);
                self.entered.add_permits(1);
                // Park while holding the inline-fallback permit.
                let _ = self.gate.acquire().await;
                self.inflight.fetch_sub(1, Ordering::SeqCst);
                self.resolve()
            })
        }
    }

    async fn ok_handler(_body: Body) -> impl IntoResponse {
        let resp = r#"{"bucket":"b","path":"p","size":1,"content_type":"application/octet-stream","generation":1}"#;
        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            resp,
        )
    }
    let app = Router::new().route("/v1/storage", post(ok_handler));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Budget = 4 permits; each task requests 2 permits → at most 2 concurrent.
    const BUDGET: usize = 4;
    const PERMITS_PER_TASK_BYTES: u64 = 2 * 1024 * 1024; // 2 MiB → 2 permits
    const EXPECTED_PEAK: u32 = 2;
    const FIRED: usize = 6;

    let entered = Arc::new(tokio::sync::Semaphore::new(0));
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let resolver = Arc::new(ConcurrencyResolver {
        inflight: Arc::new(AtomicU32::new(0)),
        peak: Arc::new(AtomicU32::new(0)),
        started: Arc::new(AtomicU32::new(0)),
        entered: entered.clone(),
        gate: gate.clone(),
        proxy_base_url: format!("http://{}/v1", addr),
    });
    let peak = resolver.peak.clone();
    let started = resolver.started.clone();

    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    // A small real file: it is never read until after release (the resolver
    // parks first), then streamed to the fast mock server above.
    let source = temp.path().join("blob.bin");
    std::fs::write(&source, b"x").unwrap();

    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = UploadQueue {
        tx,
        queue_dir: queue_dir.clone(),
        resolver: resolver.clone(),
        stats: Arc::new(UploadQueueStats::new()),
        max_queue_bytes: DEFAULT_MAX_QUEUE_BYTES,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(BUDGET)),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    };

    // Sanity: each task's permit request is within the real clamp and the
    // test budget, so it can never deadlock.
    assert_eq!(inline_fallback_permits(PERMITS_PER_TASK_BYTES), 2);

    // Fire more tasks than the budget allows to run concurrently.
    for _ in 0..FIRED {
        queue.spawn_inline_upload_from_path(
            source.clone(),
            "gcs/path".to_string(),
            "application/octet-stream".to_string(),
            PERMITS_PER_TASK_BYTES,
        );
    }

    // Deterministically wait until the first wave (EXPECTED_PEAK tasks) is parked.
    let _first_wave = entered
        .acquire_many(EXPECTED_PEAK)
        .await
        .expect("entered semaphore not closed");
    assert_eq!(
        resolver.inflight.load(Ordering::SeqCst),
        EXPECTED_PEAK,
        "exactly the budget's worth of tasks should be in-flight"
    );

    // No additional task may enter while the budget is saturated: a bounded
    // wait for one more "entered" permit must time out.
    let extra = tokio::time::timeout(Duration::from_millis(300), entered.acquire()).await;
    assert!(
        extra.is_err(),
        "no task beyond the permit budget may run concurrently"
    );
    assert!(
        peak.load(Ordering::SeqCst) <= EXPECTED_PEAK,
        "peak concurrency {} exceeded the permit budget {}",
        peak.load(Ordering::SeqCst),
        EXPECTED_PEAK
    );

    // Release all parked tasks; the excess tasks now make progress in waves.
    gate.add_permits(FIRED);

    for _ in 0..200 {
        if started.load(Ordering::SeqCst) as usize >= FIRED {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        started.load(Ordering::SeqCst) as usize,
        FIRED,
        "all fired tasks must eventually run once permits free up"
    );
    assert!(
        peak.load(Ordering::SeqCst) <= EXPECTED_PEAK,
        "peak concurrency {} must never exceed the permit budget {} across all waves",
        peak.load(Ordering::SeqCst),
        EXPECTED_PEAK
    );
}

// ---- Reference-based queue items ----

/// An axum app whose `/v1/storage` handler returns 200 + a parseable upload
/// response and counts requests. Returns `(resolver, request_count)`.
async fn spawn_ok_server() -> (Arc<dyn TraceExportSource>, Arc<AtomicU32>) {
    use axum::{
        Router, body::Body, extract::State, http::StatusCode, response::IntoResponse, routing::post,
    };
    async fn ok_handler(State(s): State<Arc<AtomicU32>>, _body: Body) -> impl IntoResponse {
        s.fetch_add(1, Ordering::SeqCst);
        let resp = r#"{"bucket":"b","path":"p","size":1,"content_type":"application/octet-stream","generation":1}"#;
        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            resp,
        )
    }
    let count = Arc::new(AtomicU32::new(0));
    let app = Router::new()
        .route("/v1/storage", post(ok_handler))
        .with_state(count.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: Arc::new(AtomicU32::new(0)),
        proxy_base_url: format!("http://{}/v1", addr),
    });
    (resolver, count)
}

/// Build an `OwnedSnapshot` queue item directly, to test disk-budget
/// accounting deterministically regardless of whether the test FS reflinks.
fn owned_snapshot_item(
    path: PathBuf,
    disk_bytes: u64,
    completion_tx: Option<oneshot::Sender<anyhow::Result<UploadCompletion>>>,
) -> UploadQueueItem {
    UploadQueueItem {
        source: UploadSource::OwnedSnapshot { path, disk_bytes },
        gcs_path: "changes_dedup/v2/blobs/sha256_snap".to_string(),
        content_type: "application/octet-stream".to_string(),
        artifact_name: "snap".to_string(),
        attempts: 0,
        enqueued_at: Instant::now(),
        sidecar_path: None,
        completion_tx,
        client_version: None,
        compress: false,
        parent_span: tracing::Span::none(),
        _in_flight: None,
    }
}

fn test_queue(
    tx: mpsc::Sender<UploadQueueItem>,
    queue_dir: PathBuf,
    resolver: Arc<dyn TraceExportSource>,
    stats: Arc<UploadQueueStats>,
    max_queue_bytes: u64,
) -> UploadQueue {
    UploadQueue {
        tx,
        queue_dir,
        resolver,
        stats,
        max_queue_bytes,
        client_version: None,
        drain_state: Arc::new(Mutex::new(None)),
        inline_fallback_semaphore: Arc::new(tokio::sync::Semaphore::new(
            INLINE_FALLBACK_TOTAL_PERMITS as usize,
        )),
        uploads_in_flight: Arc::new(Mutex::new(HashSet::new())),
    }
}

/// CORE regression: the snapshot is immutable, so mutating the working-tree
/// source AFTER enqueue does not change the uploaded bytes. This FAILS against
/// the old verify-then-reupload-source approach (which would stream the new
/// bytes to the content-addressed `sha256_<expected>` path).
#[tokio::test]
async fn reference_snapshot_immutable_to_source_mutation() {
    use axum::{
        Router, body::Bytes, extract::State, http::StatusCode, response::IntoResponse,
        routing::post,
    };
    async fn capture(State(s): State<Arc<Mutex<Vec<u8>>>>, body: Bytes) -> impl IntoResponse {
        *s.lock().unwrap() = body.to_vec();
        let resp = r#"{"bucket":"b","path":"p","size":1,"content_type":"application/octet-stream","generation":1}"#;
        (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            resp,
        )
    }
    let captured = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/v1/storage", post(capture))
        .with_state(captured.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: Arc::new(AtomicU32::new(0)),
        proxy_base_url: format!("http://{}/v1", addr),
    });

    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("image.bin");
    let original = vec![0xABu8; 4096];
    std::fs::write(&source, &original).unwrap();
    let sha = crate::sha256_hex_from_file(&source, None).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = test_queue(
        tx,
        queue_dir,
        resolver.clone(),
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let result = queue
        .enqueue_file_reference(
            &source,
            &sha,
            "gcs/p",
            "application/octet-stream",
            "dedup_x",
            "sess",
            0,
        )
        .await
        .unwrap();
    let item = rx.recv().await.expect("snapshot enqueued");
    let snapshot_path = item.source.path().to_path_buf();

    // Mutate the source AFTER the snapshot was taken; CoW/copy keeps the
    // snapshot's original bytes.
    std::fs::write(&source, vec![0xFFu8; 4096]).unwrap();

    let consecutive = Arc::new(AtomicU32::new(0));
    process_item(
        item,
        &resolver,
        &UploadRetryPolicy::default(),
        &stats,
        &consecutive,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await;

    assert!(matches!(result.completion_rx.await, Ok(Ok(_))));
    assert_eq!(
        *captured.lock().unwrap(),
        original,
        "uploaded bytes are the immutable snapshot, not the mutated source"
    );
    assert!(source.exists(), "original working-tree source untouched");
    assert!(
        !snapshot_path.exists(),
        "owned snapshot deleted after upload"
    );
    assert_eq!(stats.uploaded.load(Ordering::Relaxed), 1);
}

/// Source changed before the snapshot (sim: `expected_sha256` doesn't match
/// current content) → stale skip: nothing enqueued, completion resolves Err,
/// `reference_stale` bumps, source preserved, snapshot removed.
#[tokio::test]
async fn reference_snapshot_stale_at_enqueue_is_skipped() {
    let (resolver, request_count) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("image.bin");
    std::fs::write(&source, vec![0x11u8; 4096]).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = test_queue(
        tx,
        queue_dir.clone(),
        resolver,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let result = queue
        .enqueue_file_reference(
            &source,
            &"0".repeat(64), // wrong sha → snapshot won't match
            "gcs/p",
            "application/octet-stream",
            "dedup_x",
            "sess",
            0,
        )
        .await
        .unwrap();

    assert!(
        matches!(result.completion_rx.await, Ok(Err(_))),
        "stale snapshot resolves Err"
    );
    assert!(
        rx.try_recv().is_err(),
        "nothing enqueued for a stale snapshot"
    );
    // `reference_stale == 1` only fires AFTER the snapshot was created and
    // hashed, so this confirms created-then-cleaned (not never-created).
    assert_eq!(stats.reference_stale.load(Ordering::Relaxed), 1);
    assert_eq!(request_count.load(Ordering::SeqCst), 0, "never uploaded");
    assert!(source.exists(), "source preserved");
    let leftover: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert!(leftover.is_empty(), "stale snapshot deleted from queue dir");
}

/// The snapshot's bytes equal the source — reflink and copy-fallback both
/// produce correct content regardless of FS support.
#[tokio::test]
async fn reference_snapshot_content_matches_source() {
    let (resolver, _rc) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("image.bin");
    let bytes: Vec<u8> = (0u32..5000).map(|i| i as u8).collect();
    std::fs::write(&source, &bytes).unwrap();
    let sha = crate::sha256_hex_from_file(&source, None).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = test_queue(tx, queue_dir, resolver, stats, DEFAULT_MAX_QUEUE_BYTES);
    queue
        .enqueue_file_reference(
            &source,
            &sha,
            "gcs/p",
            "application/octet-stream",
            "dedup_x",
            "sess",
            0,
        )
        .await
        .unwrap();
    let item = rx.recv().await.expect("snapshot enqueued");
    assert_eq!(
        std::fs::read(item.source.path()).unwrap(),
        bytes,
        "snapshot bytes equal the source"
    );
}

/// A reflink snapshot (`disk_bytes == 0`) contributes 0 to the budget gauge:
/// `process_item` subtracts 0, leaving `pending_bytes` at its primed value.
#[tokio::test]
async fn owned_snapshot_reflink_zero_disk_bytes_not_budget_counted() {
    let (resolver, _rc) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let snap = temp.path().join("snap.bin");
    std::fs::write(&snap, vec![0x11u8; 4096]).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    stats.pending_bytes.store(7_000, Ordering::Relaxed); // primed by other items
    let consecutive = Arc::new(AtomicU32::new(0));
    let item = owned_snapshot_item(snap, 0, None);
    process_item(
        item,
        &resolver,
        &UploadRetryPolicy::default(),
        &stats,
        &consecutive,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await;

    assert_eq!(stats.uploaded.load(Ordering::Relaxed), 1);
    assert_eq!(
        stats.pending_bytes.load(Ordering::Relaxed),
        7_000,
        "reflink snapshot subtracts 0 disk bytes"
    );
}

/// A copy-fallback snapshot (`disk_bytes == size`) IS counted: `process_item`
/// subtracts exactly its `disk_bytes`. The real copy-fallback BRANCH in
/// `enqueue_file_reference` (`reflink_or_copy` → `Ok(Some(n))`) only fires on a
/// non-CoW FS, which the test FS isn't; this construction-shortcut test is the
/// deterministic coverage for that branch's accounting.
#[tokio::test]
async fn owned_snapshot_copy_disk_bytes_counted() {
    let (resolver, _rc) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let snap = temp.path().join("snap.bin");
    std::fs::write(&snap, vec![0x11u8; 4096]).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    stats.pending_bytes.store(4_096, Ordering::Relaxed); // this copy's bytes
    let consecutive = Arc::new(AtomicU32::new(0));
    let item = owned_snapshot_item(snap, 4_096, None);
    process_item(
        item,
        &resolver,
        &UploadRetryPolicy::default(),
        &stats,
        &consecutive,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await;

    assert_eq!(
        stats.pending_bytes.load(Ordering::Relaxed),
        0,
        "copy snapshot subtracts its disk bytes"
    );
}

/// `check_snapshot` keeps the three outcomes distinct: a match → `Match`; a
/// hash mismatch and a missing (NotFound) snapshot → `Stale`; and a transient
/// read error (reading a directory as a file, non-NotFound on Linux/macOS) →
/// `Io`. Mutation-resistant: collapsing `Io` into `Stale` (`Io` is mapped
/// to `failed`, not `reference_stale`) fails this test.
#[test]
fn check_snapshot_classifies_io_distinct_from_stale() {
    let temp = tempfile::TempDir::new().unwrap();
    let file = temp.path().join("blob.bin");
    std::fs::write(&file, vec![0x11u8; 4096]).unwrap();
    let sha = crate::sha256_hex_from_file(&file, None).unwrap();

    assert!(matches!(check_snapshot(&file, &sha), SnapshotCheck::Match));
    assert!(matches!(
        check_snapshot(&file, &"0".repeat(64)),
        SnapshotCheck::Stale
    ));
    assert!(matches!(
        check_snapshot(&temp.path().join("gone.bin"), &sha),
        SnapshotCheck::Stale
    ));
    // A directory yields a non-NotFound read error → transient `Io`, NOT stale.
    assert!(matches!(
        check_snapshot(temp.path(), &sha),
        SnapshotCheck::Io(_)
    ));
}

/// `snapshot_route` gates ONLY over-budget real copies: a reflink
/// (`disk_bytes == 0`) always queues even when over budget; an under-budget
/// copy queues; only an over-budget copy routes to the inline fallback.
#[test]
fn snapshot_route_gates_only_over_budget_copies() {
    assert_eq!(snapshot_route(0, true), SnapshotRoute::Queue);
    assert_eq!(snapshot_route(0, false), SnapshotRoute::Queue);
    assert_eq!(snapshot_route(4096, false), SnapshotRoute::Queue);
    assert_eq!(snapshot_route(4096, true), SnapshotRoute::InlineFallback);
}

/// On a CLOSED channel `enqueue_file_reference` falls back to a bounded inline
/// upload of the owned snapshot (mirrors `enqueue_file`): completion resolves
/// Ok, `enqueue_fallbacks` bumps, the snapshot is deleted, source preserved.
#[tokio::test]
async fn enqueue_file_reference_channel_closed_falls_back_inline() {
    let (resolver, request_count) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("image.bin");
    std::fs::write(&source, vec![0x11u8; 1024]).unwrap();
    let sha = crate::sha256_hex_from_file(&source, None).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    drop(rx); // closed channel → try_send fails → inline fallback
    let queue = test_queue(
        tx,
        queue_dir.clone(),
        resolver,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let result = queue
        .enqueue_file_reference(
            &source,
            &sha,
            "gcs/p",
            "application/octet-stream",
            "dedup_x",
            "sess",
            0,
        )
        .await
        .unwrap();

    assert!(
        matches!(result.completion_rx.await, Ok(Ok(_))),
        "closed channel falls back to a successful inline upload"
    );
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        1,
        "streamed inline once"
    );
    assert_eq!(stats.enqueue_fallbacks.load(Ordering::Relaxed), 1);
    assert!(source.exists(), "source preserved");
    let leftover: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert!(leftover.is_empty(), "snapshot deleted by inline fallback");
    assert_eq!(
        stats.pending_bytes.load(Ordering::Relaxed),
        0,
        "gauge rolled back"
    );
}

/// On a FULL channel `enqueue_file_reference` also falls back to a bounded
/// inline upload (never blocks or drops).
#[tokio::test]
async fn enqueue_file_reference_channel_full_falls_back_inline() {
    let (resolver, request_count) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("image.bin");
    std::fs::write(&source, vec![0x11u8; 1024]).unwrap();
    let sha = crate::sha256_hex_from_file(&source, None).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    // Capacity-1 channel pre-filled with a dummy item (rx kept alive so the
    // channel is FULL, not closed) → the next try_send returns Full.
    let (tx, _rx) = mpsc::channel(1);
    tx.try_send(owned_snapshot_item(temp.path().join("dummy.bin"), 0, None))
        .expect("first send fills the single slot");
    let queue = test_queue(
        tx,
        queue_dir.clone(),
        resolver,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let result = queue
        .enqueue_file_reference(
            &source,
            &sha,
            "gcs/p",
            "application/octet-stream",
            "dedup_x",
            "sess",
            0,
        )
        .await
        .unwrap();

    assert!(
        matches!(result.completion_rx.await, Ok(Ok(_))),
        "full channel falls back to a successful inline upload"
    );
    assert_eq!(
        request_count.load(Ordering::SeqCst),
        1,
        "streamed inline once"
    );
    assert_eq!(stats.enqueue_fallbacks.load(Ordering::Relaxed), 1);
    assert!(source.exists(), "source preserved");
    // Only the dummy remains in the queue dir; the reference snapshot was
    // streamed inline and deleted.
    let leftover: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert!(
        leftover.is_empty(),
        "reference snapshot deleted by inline fallback"
    );
}

/// Real enqueue → process round-trip: `pending_bytes` adds exactly the
/// snapshot's `disk_bytes` at enqueue and subtracts it at completion, back to
/// baseline (0). FS-independent — ties the add to the recorded disk_bytes
/// whether the test FS reflinks (0) or copies (size).
#[tokio::test]
async fn reference_enqueue_process_pending_bytes_round_trip() {
    let (resolver, _rc) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("image.bin");
    std::fs::write(&source, vec![0x11u8; 4096]).unwrap();
    let sha = crate::sha256_hex_from_file(&source, None).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = test_queue(
        tx,
        queue_dir,
        resolver.clone(),
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    queue
        .enqueue_file_reference(
            &source,
            &sha,
            "gcs/p",
            "application/octet-stream",
            "dedup_x",
            "sess",
            0,
        )
        .await
        .unwrap();

    let item = rx.recv().await.expect("snapshot enqueued");
    let disk_bytes = match &item.source {
        UploadSource::OwnedSnapshot { disk_bytes, .. } => *disk_bytes,
        other => panic!("expected OwnedSnapshot, got {:?}", other.path()),
    };
    assert_eq!(
        stats.pending_bytes.load(Ordering::Relaxed),
        disk_bytes,
        "enqueue added exactly the snapshot's disk_bytes"
    );

    let consecutive = Arc::new(AtomicU32::new(0));
    process_item(
        item,
        &resolver,
        &UploadRetryPolicy::default(),
        &stats,
        &consecutive,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await;

    assert_eq!(
        stats.pending_bytes.load(Ordering::Relaxed),
        0,
        "completion subtracted disk_bytes back to baseline"
    );
    assert_eq!(stats.pending.load(Ordering::Relaxed), 0);
}

/// A missing source at enqueue surfaces as `Err` (the stat fails) — no
/// snapshot is created.
#[tokio::test]
async fn enqueue_file_reference_missing_source_errors() {
    let (resolver, _rc) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let missing = temp.path().join("gone.bin");

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, _rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = test_queue(
        tx,
        queue_dir.clone(),
        resolver,
        stats,
        DEFAULT_MAX_QUEUE_BYTES,
    );

    let err = queue
        .enqueue_file_reference(
            &missing,
            &"0".repeat(64),
            "gcs/p",
            "application/octet-stream",
            "dedup_x",
            "sess",
            0,
        )
        .await;
    assert!(err.is_err(), "missing source must return Err");
    let leftover: Vec<_> = std::fs::read_dir(&queue_dir).unwrap().flatten().collect();
    assert!(
        leftover.is_empty(),
        "no snapshot created for a missing source"
    );
}

/// A 0-byte source snapshots and verifies fine (empty-file sha matches).
#[tokio::test]
async fn enqueue_file_reference_zero_byte_source_succeeds() {
    let (resolver, _rc) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();
    let source = temp.path().join("empty.bin");
    std::fs::write(&source, b"").unwrap();
    let sha = crate::sha256_hex_from_file(&source, None).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let (tx, mut rx) = mpsc::channel(CHANNEL_CAPACITY);
    let queue = test_queue(
        tx,
        queue_dir,
        resolver,
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );

    queue
        .enqueue_file_reference(
            &source,
            &sha,
            "gcs/p",
            "application/octet-stream",
            "dedup_x",
            "sess",
            0,
        )
        .await
        .unwrap();

    let item = rx.recv().await.expect("0-byte snapshot enqueued");
    assert!(matches!(item.source, UploadSource::OwnedSnapshot { .. }));
    assert_eq!(stats.reference_stale.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::metadata(item.source.path()).unwrap().len(), 0);
}

/// A retry-exhausted `process_item` deletes the owned snapshot.
#[tokio::test]
async fn process_item_owned_snapshot_failure_deletes_snapshot() {
    // 401 → fast hard failure after the single auth retry.
    use axum::{Router, body::Body, http::StatusCode, response::IntoResponse, routing::post};
    async fn h401(_b: Body) -> impl IntoResponse {
        (StatusCode::UNAUTHORIZED, "no")
    }
    let app = Router::new().route("/v1/storage", post(h401));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let resolver: Arc<dyn TraceExportSource> = Arc::new(CountingResolver {
        count: Arc::new(AtomicU32::new(0)),
        proxy_base_url: format!("http://{}/v1", addr),
    });

    let temp = tempfile::TempDir::new().unwrap();
    let snap = temp.path().join("snap.bin");
    std::fs::write(&snap, vec![0x11u8; 256]).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let consecutive = Arc::new(AtomicU32::new(0));
    let policy = UploadRetryPolicy {
        max_attempts: 5,
        initial_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        multiplier: 1.0,
        max_age: DEFAULT_MAX_AGE,
        auth_park_probe_interval: DEFAULT_AUTH_PARK_PROBE_INTERVAL,
    };
    let item = owned_snapshot_item(snap.clone(), 0, None);
    process_item(
        item,
        &resolver,
        &policy,
        &stats,
        &consecutive,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await;

    assert_eq!(stats.failed.load(Ordering::Relaxed), 1);
    assert!(!snap.exists(), "snapshot deleted after upload failure");
}

/// An expired `process_item` (age-check drop) deletes the owned snapshot.
#[tokio::test]
async fn process_item_owned_snapshot_expiry_deletes_snapshot() {
    let (resolver, request_count) = spawn_ok_server().await;
    let temp = tempfile::TempDir::new().unwrap();
    let snap = temp.path().join("snap.bin");
    std::fs::write(&snap, vec![0x11u8; 256]).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let consecutive = Arc::new(AtomicU32::new(0));
    let policy = UploadRetryPolicy {
        max_age: Duration::ZERO,
        ..Default::default()
    };
    let mut item = owned_snapshot_item(snap.clone(), 0, None);
    item.enqueued_at = Instant::now()
        .checked_sub(Duration::from_secs(3600))
        .unwrap_or_else(Instant::now);
    process_item(
        item,
        &resolver,
        &policy,
        &stats,
        &consecutive,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await;

    assert_eq!(
        request_count.load(Ordering::SeqCst),
        0,
        "expired: no upload"
    );
    assert!(!snap.exists(), "snapshot deleted on expiry");
}

/// An owned-temp item is deleted after a successful upload.
#[tokio::test]
async fn process_item_owned_temp_deleted_after_success() {
    let (resolver, request_count) = spawn_ok_server().await;

    let temp = tempfile::TempDir::new().unwrap();
    let owned = temp.path().join("owned_temp.bin");
    std::fs::write(&owned, vec![0x33u8; 256]).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let consecutive = Arc::new(AtomicU32::new(0));
    let policy = UploadRetryPolicy::default();

    let item = UploadQueueItem {
        source: UploadSource::OwnedTemp(owned.clone()),
        gcs_path: "session/turn_0/owned.bin".to_string(),
        content_type: "application/octet-stream".to_string(),
        artifact_name: "owned".to_string(),
        attempts: 0,
        enqueued_at: Instant::now(),
        sidecar_path: None,
        completion_tx: None,
        client_version: None,
        compress: false,
        parent_span: tracing::Span::none(),
        _in_flight: None,
    };
    process_item(
        item,
        &resolver,
        &policy,
        &stats,
        &consecutive,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await;

    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert_eq!(stats.uploaded.load(Ordering::Relaxed), 1);
    assert!(!owned.exists(), "owned temp must be deleted after upload");
}

/// A successful upload deletes both the temp file and its sidecar.
#[tokio::test]
async fn process_item_deletes_sidecar_with_temp_after_success() {
    let (resolver, request_count) = spawn_ok_server().await;

    let temp = tempfile::TempDir::new().unwrap();
    let owned = temp.path().join("owned_temp.bin");
    std::fs::write(&owned, vec![0x44u8; 256]).unwrap();
    let sidecar = sidecar_path_for(&owned);
    std::fs::write(&sidecar, br#"{"schema_version":1}"#).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let consecutive = Arc::new(AtomicU32::new(0));
    let policy = UploadRetryPolicy::default();

    let item = UploadQueueItem {
        source: UploadSource::OwnedTemp(owned.clone()),
        gcs_path: "session/turn_0/owned.bin".to_string(),
        content_type: "application/octet-stream".to_string(),
        artifact_name: "owned".to_string(),
        attempts: 0,
        enqueued_at: Instant::now(),
        sidecar_path: Some(sidecar.clone()),
        completion_tx: None,
        client_version: None,
        compress: false,
        parent_span: tracing::Span::none(),
        _in_flight: None,
    };
    process_item(
        item,
        &resolver,
        &policy,
        &stats,
        &consecutive,
        &Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
    )
    .await;

    assert_eq!(request_count.load(Ordering::SeqCst), 1);
    assert_eq!(stats.uploaded.load(Ordering::Relaxed), 1);
    assert!(!owned.exists(), "temp deleted after upload");
    assert!(
        !sidecar.exists(),
        "sidecar deleted together with temp after upload"
    );
}

/// The orphan sweep deletes lone temp/sidecar files and counts them as
/// mismatched.
#[test]
fn cleanup_orphans_counts_lone_files_as_mismatched() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stale = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
    let ft = filetime::FileTime::from_system_time(stale);

    // Lone temp file (no matching sidecar).
    let lone_tmp = queue_dir.join("aa_turn0_before_changes.tar.gz_1_0");
    std::fs::write(&lone_tmp, b"orphan archive").unwrap();
    filetime::set_file_mtime(&lone_tmp, ft).unwrap();

    // Lone sidecar (no matching temp file).
    let lone_sidecar = queue_dir.join("bb_turn0_after_changes.tar.gz_2_0.meta.json");
    std::fs::write(&lone_sidecar, b"{}").unwrap();
    filetime::set_file_mtime(&lone_sidecar, ft).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let queue = test_queue(
        mpsc::channel(1).0,
        queue_dir.clone(),
        Arc::new(MockResolver),
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );
    queue.cleanup_orphans(Duration::from_secs(3600));

    assert!(!lone_tmp.exists(), "lone temp swept");
    assert!(!lone_sidecar.exists(), "lone sidecar swept");
    assert_eq!(
        stats.cleanup_orphan_mismatched.load(Ordering::Relaxed),
        2,
        "both lone files counted as mismatched"
    );
}

/// A stale matched temp+sidecar pair is swept but not counted as mismatched.
#[test]
fn cleanup_orphans_does_not_count_matched_pair() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let stale = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
    let ft = filetime::FileTime::from_system_time(stale);

    let tmp = queue_dir.join("cc_turn1_before_changes.tar.gz_3_0");
    std::fs::write(&tmp, b"paired archive").unwrap();
    filetime::set_file_mtime(&tmp, ft).unwrap();
    let sidecar = sidecar_path_for(&tmp);
    std::fs::write(&sidecar, b"{}").unwrap();
    filetime::set_file_mtime(&sidecar, ft).unwrap();

    let stats = Arc::new(UploadQueueStats::new());
    let queue = test_queue(
        mpsc::channel(1).0,
        queue_dir.clone(),
        Arc::new(MockResolver),
        stats.clone(),
        DEFAULT_MAX_QUEUE_BYTES,
    );
    queue.cleanup_orphans(Duration::from_secs(3600));

    assert!(!tmp.exists(), "stale temp removed");
    assert!(!sidecar.exists(), "stale sidecar removed");
    assert_eq!(
        stats.cleanup_orphan_mismatched.load(Ordering::Relaxed),
        0,
        "a matched pair must not be counted as mismatched"
    );
}

/// The janitor derives a pair's age from the sidecar's `enqueued_at` (same
/// source as the recovery scan), falling back to mtime only when no
/// parseable sidecar exists. mtime and `enqueued_at` disagreeing must not
/// produce a deletion recovery would have disagreed with.
#[test]
fn cleanup_orphans_uses_sidecar_age_for_pairs() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    let make_pair = |stem: &str, enqueued_at: chrono::DateTime<chrono::Utc>, old_mtime| {
        let tmp = queue_dir.join(stem);
        std::fs::write(&tmp, b"bytes").unwrap();
        let sidecar = QueueItemSidecar {
            schema_version: 1,
            session_id: "s".to_string(),
            turn_number: 1,
            gcs_path: "s/turn_1/a".to_string(),
            content_type: "application/gzip".to_string(),
            artifact_name: "a".to_string(),
            enqueued_at: enqueued_at.to_rfc3339(),
            sha256: "0".repeat(64),
        };
        let sc = sidecar_path_for(&tmp);
        std::fs::write(&sc, serde_json::to_vec(&sidecar).unwrap()).unwrap();
        if old_mtime {
            let stale = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
            let ft = filetime::FileTime::from_system_time(stale);
            filetime::set_file_mtime(&tmp, ft).unwrap();
            filetime::set_file_mtime(&sc, ft).unwrap();
        }
        (tmp, sc)
    };

    // Old mtime but fresh enqueued_at: recovery would keep it → janitor must too.
    let (keep_tmp, keep_sc) = make_pair("aa_turn1_keep.tar.gz_1_0", chrono::Utc::now(), true);
    // Fresh mtime but expired enqueued_at: recovery would drop it → janitor may too.
    let (drop_tmp, drop_sc) = make_pair(
        "bb_turn1_drop.tar.gz_2_0",
        chrono::Utc::now() - chrono::Duration::hours(3),
        false,
    );

    cleanup_queue_dir(&queue_dir, Duration::from_secs(2 * 3600), None);

    assert!(keep_tmp.exists(), "fresh-by-sidecar temp kept");
    assert!(keep_sc.exists(), "fresh-by-sidecar sidecar kept");
    assert!(!drop_tmp.exists(), "expired-by-sidecar temp removed");
    assert!(!drop_sc.exists(), "expired-by-sidecar sidecar removed");
}

/// `remove_owned_source` deletes both variants — both are queue-owned (a
/// working-tree source is snapshotted, never enqueued directly).
#[test]
fn remove_owned_source_deletes_both_variants() {
    let temp = tempfile::TempDir::new().unwrap();

    let owned_path = temp.path().join("owned.bin");
    std::fs::write(&owned_path, b"owned").unwrap();
    remove_owned_source(&UploadSource::OwnedTemp(owned_path.clone()), None);
    assert!(!owned_path.exists(), "owned temp should be removed");

    let snap_path = temp.path().join("snap.bin");
    std::fs::write(&snap_path, b"snapshot").unwrap();
    remove_owned_source(
        &UploadSource::OwnedSnapshot {
            path: snap_path.clone(),
            disk_bytes: 0,
        },
        None,
    );
    assert!(!snap_path.exists(), "owned snapshot should be removed");
}

/// The orphan sweep only touches the queue dir; a stale working-tree
/// reference source living outside it is never deleted.
#[test]
fn cleanup_orphans_never_deletes_reference_source() {
    let temp = tempfile::TempDir::new().unwrap();
    let queue_dir = temp.path().join("upload_queue");
    std::fs::create_dir_all(&queue_dir).unwrap();

    // Reference source lives in the working tree (outside queue_dir) and is old.
    let worktree = temp.path().join("worktree");
    std::fs::create_dir_all(&worktree).unwrap();
    let ref_source = worktree.join("image.bin");
    std::fs::write(&ref_source, b"durable working-tree file").unwrap();
    let three_hours_ago = std::time::SystemTime::now() - Duration::from_secs(3 * 3600);
    let ft = filetime::FileTime::from_system_time(three_hours_ago);
    filetime::set_file_mtime(&ref_source, ft).unwrap();

    let cleaned = cleanup_orphaned_uploads(temp.path(), Duration::from_secs(3600));
    assert_eq!(cleaned, 0, "nothing in queue_dir to clean");
    assert!(
        ref_source.exists(),
        "a reference source outside queue_dir must never be swept"
    );
}
