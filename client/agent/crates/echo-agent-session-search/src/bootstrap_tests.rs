//! Gate tests inject a fresh BootstrapProgress and assert only on their own
//! per-tmpdir database state.

use super::*;
use crate::fts::META_KEY_SCHEMA_VERSION;

/// A synthetic session store: the gate tests exercise the claim lease, not
/// the on-disk layout, so the sessions need no transcripts. `list_sessions`
/// still has to report a non-zero count for the single-flight test, which
/// asserts that exactly one of two racing gates ran the reindex.
struct FakeSource {
    sessions: Vec<IndexableSession>,
    /// Latched when a gate task returns. `list_sessions` runs only in the
    /// gate that won the claim, so waiting on this holds the lease until the
    /// other gate has finished its whole wait window. Without it the fake
    /// enumerates so fast that the loser's first claim attempt can land
    /// after the winner already released, and a launch's first claim always
    /// reindexes.
    peer_done: Option<Arc<AtomicBool>>,
}

/// Bounds the hold in [`FakeSource::list_sessions`] so a bug cannot hang CI.
const PEER_DONE_TIMEOUT: Duration = Duration::from_secs(10);

impl FakeSource {
    fn empty() -> Self {
        Self {
            sessions: Vec::new(),
            peer_done: None,
        }
    }

    fn with_ids(ids: &[&str], peer_done: Arc<AtomicBool>) -> Self {
        Self {
            sessions: ids
                .iter()
                .map(|id| IndexableSession {
                    session_id: (*id).to_string(),
                    cwd: "/ws".to_string(),
                    updated_at_unix: 1_700_000_000,
                    title: format!("session {id}"),
                    updates_path: None,
                })
                .collect(),
            peer_done: Some(peer_done),
        }
    }
}

#[async_trait::async_trait]
impl SessionSource for FakeSource {
    async fn list_sessions(&self) -> io::Result<Vec<IndexableSession>> {
        if let Some(peer_done) = &self.peer_done {
            let deadline = Instant::now() + PEER_DONE_TIMEOUT;
            while !peer_done.load(Ordering::Acquire) && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
        Ok(self.sessions.clone())
    }

    async fn load_session(
        &self,
        session_id: &str,
        _cwd: &str,
    ) -> io::Result<Option<IndexableSession>> {
        Ok(self
            .sessions
            .iter()
            .find(|s| s.session_id == session_id)
            .cloned())
    }
}

fn no_content(_path: &Path) -> io::Result<(String, u64)> {
    Ok((String::new(), 0))
}

fn write_last_bootstrap_at(db_path: &Path) -> io::Result<()> {
    let now = chrono::Utc::now().timestamp();
    with_search_index(db_path, |index| {
        index.set_meta(META_KEY_LAST_BOOTSTRAP, &now.to_string())
    })
}

const TEST_TIMING: BootstrapTiming = BootstrapTiming {
    lease: Duration::from_secs(300),
    refresh: Duration::from_millis(50),
    peer_wait: Duration::from_millis(200),
    poll: Duration::from_millis(10),
};
const _: () = assert!(TEST_TIMING.refresh.as_millis() < TEST_TIMING.lease.as_millis());
const _: () = assert!(TEST_TIMING.poll.as_millis() < TEST_TIMING.peer_wait.as_millis());

/// [`TEST_TIMING`] with a shorter peer wait, for the single-flight test. That
/// test holds the winning gate's claim open for the losing gate's whole wait,
/// and the shorter the hold the smaller the window in which a sibling test
/// bumps the process-global cache epoch (see the marker assertion there).
const CONTENDED_TIMING: BootstrapTiming = BootstrapTiming {
    lease: Duration::from_secs(300),
    refresh: Duration::from_millis(50),
    peer_wait: Duration::from_millis(60),
    poll: Duration::from_millis(5),
};
const _: () = assert!(CONTENDED_TIMING.refresh.as_millis() < CONTENDED_TIMING.lease.as_millis());
const _: () = assert!(CONTENDED_TIMING.poll.as_millis() < CONTENDED_TIMING.peer_wait.as_millis());

fn stamp_marker(db_path: &Path, value: &str) {
    with_search_index(db_path, |index| {
        index.set_meta(META_KEY_LAST_BOOTSTRAP, value)
    })
    .unwrap();
}

fn read_marker(db_path: &Path) -> Option<String> {
    with_search_index(db_path, |index| index.get_meta(META_KEY_LAST_BOOTSTRAP)).unwrap()
}

#[tokio::test]
async fn test_claimant_reindexes_even_when_marker_exists() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = search_db_path(tmp.path());
    stamp_marker(&db_path, "123");

    let source = FakeSource::empty();
    bootstrap_with_lease_inner(
        tmp.path(),
        &source,
        no_content,
        &Arc::new(BootstrapProgress::default()),
        &TEST_TIMING,
        BootstrapRole::Launch,
    )
    .await
    .unwrap();

    // The reindex rewrote the marker and released the claim.
    assert_ne!(read_marker(&db_path).as_deref(), Some("123"));
    let claim =
        with_search_index(&db_path, |index| index.get_meta(META_KEY_BOOTSTRAP_CLAIM)).unwrap();
    assert_eq!(claim, None);
}

#[tokio::test]
async fn test_has_completed_bootstrap_marker_lifecycle() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path();
    let db_path = search_db_path(root);

    assert_eq!(has_completed_bootstrap_marker(root).await, Some(false));

    write_last_bootstrap_at(&db_path).unwrap();
    assert_eq!(has_completed_bootstrap_marker(root).await, Some(true));

    // An older binary re-stamped a downgraded schema version.
    {
        let index = SessionSearchIndex::open_or_create(&db_path).unwrap();
        index.set_meta(META_KEY_SCHEMA_VERSION, "3").unwrap();
    }
    assert_eq!(
        has_completed_bootstrap_marker(root).await,
        Some(false),
        "a downgraded index must not count as bootstrapped even with a recent marker"
    );

    write_last_bootstrap_at(&db_path).unwrap();
    assert_eq!(has_completed_bootstrap_marker(root).await, Some(true));
}

#[tokio::test]
async fn test_waiter_adopts_peer_marker_without_reindexing() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = search_db_path(tmp.path());

    let now = chrono::Utc::now().timestamp();
    with_search_index(&db_path, |index| {
        index.try_claim_bootstrap(now, TEST_TIMING.lease, "peer")
    })
    .unwrap();
    stamp_marker(&db_path, "123");

    let source = FakeSource::empty();
    bootstrap_with_lease_inner(
        tmp.path(),
        &source,
        no_content,
        &Arc::new(BootstrapProgress::default()),
        &TEST_TIMING,
        BootstrapRole::Launch,
    )
    .await
    .unwrap();

    assert_eq!(read_marker(&db_path).as_deref(), Some("123"));
}

#[tokio::test]
async fn test_try_bootstrap_returns_at_once_when_peer_holds_claim() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = search_db_path(tmp.path());

    let now = chrono::Utc::now().timestamp();
    with_search_index(&db_path, |index| {
        index.try_claim_bootstrap(now, TIMING.lease, "peer")
    })
    .unwrap();

    let source = FakeSource::empty();
    let started = std::time::Instant::now();
    try_bootstrap_with_lease(
        tmp.path(),
        &source,
        no_content,
        &Arc::new(BootstrapProgress::default()),
    )
    .await
    .unwrap();

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "a held claim must not block the recheck for the full peer wait"
    );
    assert_eq!(read_marker(&db_path), None, "no reindex ran");
}

#[tokio::test]
async fn test_recheck_adopts_marker_completed_after_its_probe() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = search_db_path(tmp.path());
    // A peer finished and released between the recheck's marker probe
    // and its claim attempt: the marker exists and the lease is free.
    stamp_marker(&db_path, "123");

    let source = FakeSource::empty();
    try_bootstrap_with_lease(
        tmp.path(),
        &source,
        no_content,
        &Arc::new(BootstrapProgress::default()),
    )
    .await
    .unwrap();

    assert_eq!(
        read_marker(&db_path).as_deref(),
        Some("123"),
        "the recheck must adopt the fresh marker, not reindex over it"
    );
    assert!(!has_bootstrap_claim(&db_path).unwrap());
}

#[tokio::test]
async fn test_waiter_gives_up_after_peer_wait() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = search_db_path(tmp.path());

    let now = chrono::Utc::now().timestamp();
    with_search_index(&db_path, |index| {
        index.try_claim_bootstrap(now, TEST_TIMING.lease, "peer")
    })
    .unwrap();

    let source = FakeSource::empty();
    bootstrap_with_lease_inner(
        tmp.path(),
        &source,
        no_content,
        &Arc::new(BootstrapProgress::default()),
        &TEST_TIMING,
        BootstrapRole::Launch,
    )
    .await
    .unwrap();

    assert_eq!(read_marker(&db_path), None, "no reindex ran");
}

#[test]
fn test_shared_index_reopens_after_epoch_change() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = search_db_path(tmp.path());
    let shared = SharedIndex::new();
    shared
        .with(&db_path, |index| index.set_meta("k", "v"))
        .unwrap();

    // A heal bumps the epoch and replaces the file.
    recovery::heal_unusable(
        &db_path,
        &rusqlite::Error::QueryReturnedNoRows,
        |_| Ok(false),
        |p| SessionSearchIndex::open_or_create(p).map(|_| ()),
    );

    let value = shared.with(&db_path, |index| index.get_meta("k")).unwrap();
    assert_eq!(
        value, None,
        "the connection must re-open at the new epoch, not keep the old fd"
    );
}

#[test]
fn test_read_write_last_bootstrap_at() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("session_search.sqlite");

    assert_eq!(try_read_last_bootstrap_at(&db_path).unwrap(), None);
    write_last_bootstrap_at(&db_path).unwrap();

    let ts = try_read_last_bootstrap_at(&db_path).unwrap().unwrap();
    let now = chrono::Utc::now().timestamp();
    assert!((now - ts).abs() < 5);
}

#[test]
fn test_clear_last_bootstrap_at() {
    let tmp = tempfile::TempDir::new().unwrap();
    let db_path = tmp.path().join("session_search.sqlite");

    write_last_bootstrap_at(&db_path).unwrap();
    assert!(try_read_last_bootstrap_at(&db_path).unwrap().is_some());

    clear_last_bootstrap_at(&db_path).unwrap();
    assert_eq!(try_read_last_bootstrap_at(&db_path).unwrap(), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_concurrent_gates_single_flight() {
    let tmp = tempfile::TempDir::new().unwrap();
    let root = tmp.path().to_path_buf();
    with_search_index(&search_db_path(&root), |_| Ok(())).unwrap();

    let progress_a = Arc::new(BootstrapProgress::default());
    let progress_b = Arc::new(BootstrapProgress::default());
    // Whichever gate loses the claim returns first and latches this, which
    // releases the winner's enumeration.
    let peer_done = Arc::new(AtomicBool::new(false));
    let source_a = FakeSource::with_ids(&["s1", "s2"], Arc::clone(&peer_done));
    let source_b = FakeSource::with_ids(&["s1", "s2"], Arc::clone(&peer_done));
    let root_a = root.clone();
    let root_b = root;
    let pa = Arc::clone(&progress_a);
    let pb = Arc::clone(&progress_b);
    let done_a = Arc::clone(&peer_done);
    let done_b = Arc::clone(&peer_done);
    let start = Arc::new(tokio::sync::Barrier::new(2));
    let start_a = Arc::clone(&start);
    let start_b = Arc::clone(&start);
    let epoch_before = recovery::current_epoch();
    let (a, b) = tokio::join!(
        tokio::spawn(async move {
            start_a.wait().await;
            let result = bootstrap_with_lease_inner(
                &root_a,
                &source_a,
                no_content,
                &pa,
                &CONTENDED_TIMING,
                BootstrapRole::Launch,
            )
            .await;
            done_a.store(true, Ordering::Release);
            result
        }),
        tokio::spawn(async move {
            start_b.wait().await;
            let result = bootstrap_with_lease_inner(
                &root_b,
                &source_b,
                no_content,
                &pb,
                &CONTENDED_TIMING,
                BootstrapRole::Launch,
            )
            .await;
            done_b.store(true, Ordering::Release);
            result
        }),
    );
    let a = a.expect("gate a task panicked");
    let b = b.expect("gate b task panicked");
    assert!(a.is_ok(), "gate a: {a:?}");
    assert!(b.is_ok(), "gate b: {b:?}");

    let db_path = search_db_path(tmp.path());
    // The cache epoch is process-global, so a sibling test healing its own
    // cache while these gates run makes the winner withhold its completion
    // marker by design ("cache healed during bootstrap"). That is the
    // behavior under test elsewhere; here it just means the marker is
    // legitimately absent.
    let healed = recovery::current_epoch() != epoch_before;
    assert!(
        healed || read_marker(&db_path).is_some(),
        "completion marker must exist after concurrent gates"
    );
    assert!(
        !has_bootstrap_claim(&db_path).unwrap(),
        "claim must be released after concurrent gates"
    );

    let a_ran = progress_a.total.load(Ordering::Relaxed) > 0;
    let b_ran = progress_b.total.load(Ordering::Relaxed) > 0;
    assert_eq!(
        usize::from(a_ran) + usize::from(b_ran),
        1,
        "exactly one gate must reindex, a_total={}, b_total={}",
        progress_a.total.load(Ordering::Relaxed),
        progress_b.total.load(Ordering::Relaxed),
    );
}
