//! Session search orchestration: querying and background indexing.
//!
//! The FTS index is bootstrapped on first search and updated per session via
//! [`SearchIndexManager::enqueue`]. The SQLite DB is shared with other echoagent
//! processes (older binaries may wipe or downgrade it on open), so every
//! search re-verifies the on-disk completed-bootstrap marker, and the
//! bootstrap itself is cross-process single-flight.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::bootstrap::{
    BootstrapOutcome, BootstrapProgress, BootstrappingGuard, bootstrap_with_lease,
    has_completed_bootstrap_marker, try_bootstrap_with_lease,
};
use crate::db::{
    HealAwareLogCounter, log_session_index_failure, search_db_path, search_index_exists,
    with_search_index, with_search_index_blocking,
};
use crate::doc::{UpsertOutcome, build_session_doc, upsert_unless_unchanged};
use crate::fts::SessionSearchRow;
#[cfg(test)]
use crate::fts::{META_KEY_SCHEMA_VERSION, SessionSearchIndex};
use crate::recovery;
use crate::source::{ContentExtractor, IndexableSession, SessionSource, SessionSourceFactory};

const SEARCH_INDEX_DEBOUNCE_MS: u64 = 500;
const BOOTSTRAP_WAIT_TIMEOUT: Duration = Duration::from_secs(5);
const BOOTSTRAP_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Internal search request (deserialized from the ACP extension params).
#[derive(Debug, Clone)]
pub struct SessionSearchRequest {
    pub query: String,
    pub cwd: Option<String>,
    pub limit: usize,
    pub offset: usize,
    pub include_content: bool,
}

/// Raw search response returned to the ACP extension handler.
#[derive(Debug, Clone)]
pub struct SessionSearchResponse {
    pub results: Vec<SessionSearchRow>,
    pub next_offset: Option<usize>,
    pub total_estimate: Option<usize>,
    /// True while the index is still bootstrapping; callers should re-query.
    /// Also true when a live claim exists without a completion marker, so a
    /// peer mid-rebuild or a dead claimant within its lease is visible.
    pub bootstrapping: bool,
}

impl SessionSearchResponse {
    /// Empty, and not a final answer: the caller should ask again.
    pub fn still_settling() -> Self {
        Self {
            bootstrapping: true,
            ..Self::empty()
        }
    }

    fn empty() -> Self {
        Self {
            results: Vec::new(),
            next_offset: None,
            total_estimate: Some(0),
            bootstrapping: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct SessionSearchKey {
    session_id: String,
    cwd: String,
}

enum SearchIndexJob {
    Upsert(SessionSearchKey),
    BootstrapAll,
    /// Re-verify the on-disk completed-bootstrap marker; re-run the full
    /// bootstrap when it is missing.
    RecheckBootstrap,
    RefreshSubstrings,
}

enum SearchManagerCmd {
    Enqueue { root: PathBuf, job: SearchIndexJob },
    BootstrapOnce { root: PathBuf },
}

struct SearchManagerState {
    workers: HashMap<PathBuf, mpsc::UnboundedSender<SearchIndexJob>>,
    bootstrapped: HashSet<PathBuf>,
}

/// What a spawned per-root worker needs: the session store binding and a way
/// to re-enqueue itself (a heal mid-bootstrap asks for another run).
struct WorkerContext {
    tx: mpsc::UnboundedSender<SearchManagerCmd>,
    progress: Arc<BootstrapProgress>,
    source_factory: SessionSourceFactory,
    extract: ContentExtractor,
}

impl WorkerContext {
    /// Requeue from inside a worker.
    fn bootstrap_once(&self, root: PathBuf) {
        self.progress.begin_bootstrapping();
        let _ = self.tx.send(SearchManagerCmd::BootstrapOnce { root });
    }

    fn refresh_substrings(&self, root: &Path) {
        let _ = self.tx.send(SearchManagerCmd::Enqueue {
            root: root.to_path_buf(),
            job: SearchIndexJob::RefreshSubstrings,
        });
    }
}

/// Manages background session indexing for every echoagent home this process
/// touches.
///
/// Requires an active tokio runtime on construction (spawns tasks).
pub struct SearchIndexManager {
    tx: mpsc::UnboundedSender<SearchManagerCmd>,
    progress: Arc<BootstrapProgress>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchIndexStatus {
    pub bootstrapping: bool,
    pub indexed: u64,
    pub total: u64,
    /// Sessions skipped due to size limit or timeout.
    pub skipped: u64,
    /// Sessions skipped because content hash was unchanged.
    pub unchanged: u64,
}

impl SearchIndexManager {
    /// Start the dispatcher. `source_factory` opens the session store for a echoagent home and
    /// `extract` pulls searchable text out of one transcript.
    pub fn start(source_factory: SessionSourceFactory, extract: ContentExtractor) -> Self {
        let progress = Arc::new(BootstrapProgress::default());
        let (tx, mut rx) = mpsc::unbounded_channel::<SearchManagerCmd>();
        let context = Arc::new(WorkerContext {
            tx: tx.clone(),
            progress: Arc::clone(&progress),
            source_factory,
            extract,
        });

        tokio::spawn(async move {
            let mut state = SearchManagerState {
                workers: HashMap::new(),
                bootstrapped: HashSet::new(),
            };
            while let Some(cmd) = rx.recv().await {
                match cmd {
                    SearchManagerCmd::Enqueue { root, job } => {
                        Self::dispatch(&mut state, &context, root, job);
                    }
                    SearchManagerCmd::BootstrapOnce { root } => {
                        if state.bootstrapped.insert(root.clone()) {
                            Self::dispatch(
                                &mut state,
                                &context,
                                root,
                                SearchIndexJob::BootstrapAll,
                            );
                        } else {
                            // The DB is shared: re-verify the on-disk marker,
                            // sequenced after any in-flight BootstrapAll.
                            Self::dispatch(
                                &mut state,
                                &context,
                                root,
                                SearchIndexJob::RecheckBootstrap,
                            );
                        }
                    }
                }
            }
        });

        Self { tx, progress }
    }

    /// Queue a bootstrap of all sessions (idempotent per root; repeat calls re-verify the
    /// on-disk marker). Sets `bootstrapping` eagerly so pollers see `true` before the background
    /// task starts.
    pub fn bootstrap_once(&self, root: PathBuf) {
        self.progress.begin_bootstrapping();
        let _ = self.tx.send(SearchManagerCmd::BootstrapOnce { root });
    }

    pub fn status(&self) -> SearchIndexStatus {
        SearchIndexStatus {
            bootstrapping: self.progress.is_bootstrapping(),
            indexed: self.progress.indexed.load(Ordering::Relaxed),
            total: self.progress.total.load(Ordering::Relaxed),
            skipped: self.progress.skipped.load(Ordering::Relaxed),
            unchanged: self.progress.unchanged.load(Ordering::Relaxed),
        }
    }

    pub fn enqueue(&self, root: PathBuf, session_id: String, cwd: String) {
        let key = SessionSearchKey { session_id, cwd };
        let _ = self.tx.send(SearchManagerCmd::Enqueue {
            root,
            job: SearchIndexJob::Upsert(key),
        });
    }

    fn dispatch(
        state: &mut SearchManagerState,
        context: &Arc<WorkerContext>,
        root: PathBuf,
        job: SearchIndexJob,
    ) {
        let sender = state.workers.entry(root.clone()).or_insert_with(|| {
            let (tx, rx) = mpsc::unbounded_channel();
            let root_owned = root.clone();
            let context = Arc::clone(context);
            tokio::spawn(async move {
                let source = (context.source_factory)(root_owned.clone());
                run_worker(&root_owned, source.as_ref(), &context, rx).await;
            });
            tx
        });
        if sender.send(job).is_err() {
            tracing::warn!("search worker channel closed");
        }
    }
}

/// Execute a session search query, waiting up to [`BOOTSTRAP_WAIT_TIMEOUT`]
/// for a first-call bootstrap so the query runs against a populated index.
pub async fn execute_search(
    manager: Option<&SearchIndexManager>,
    root_dir: &Path,
    req: &SessionSearchRequest,
) -> io::Result<SessionSearchResponse> {
    let query = req.query.trim();
    if query.is_empty() {
        return Ok(SessionSearchResponse::empty());
    }
    let Some(manager) = manager else {
        return Ok(SessionSearchResponse::empty());
    };

    manager.bootstrap_once(root_dir.to_path_buf());

    let epoch = recovery::CacheEpoch::now();
    let deadline = tokio::time::Instant::now() + BOOTSTRAP_WAIT_TIMEOUT;
    while manager.progress.is_bootstrapping() {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(BOOTSTRAP_POLL_INTERVAL).await;
    }
    let db_path = search_db_path(root_dir);
    let cwd = req.cwd.clone();
    let limit = req.limit;
    let offset = req.offset;
    let include_content = req.include_content;
    let query_owned = query.to_string();

    let (query_result, index_pending) = tokio::task::spawn_blocking(move || {
        with_search_index(&db_path, |index| {
            index.query_with_progress(&query_owned, cwd.as_deref(), limit, offset, include_content)
        })
    })
    .await
    .map_err(io::Error::other)??;

    let healed = epoch.changed();
    if healed {
        manager.bootstrap_once(root_dir.to_path_buf());
    }

    Ok(SessionSearchResponse {
        results: query_result.results,
        next_offset: query_result.next_offset,
        total_estimate: query_result.total_estimate,
        bootstrapping: healed || manager.progress.is_bootstrapping() || index_pending,
    })
}

/// Remove one session from an index built earlier, whether or not this process
/// indexes. Best effort: a failure is logged, not returned.
pub async fn evict_session(root_dir: &Path, session_id: &str) {
    if !search_index_exists(root_dir) {
        return;
    }
    let id = session_id.to_string();
    let deleted = with_search_index_blocking(&search_db_path(root_dir), move |index| {
        index.delete_doc(&id)
    })
    .await;
    if let Err(e) = deleted {
        log_session_index_failure(session_id, &e, "failed to remove session from search index");
    }
}

async fn run_worker(
    root_dir: &Path,
    source: &dyn SessionSource,
    context: &WorkerContext,
    mut rx: mpsc::UnboundedReceiver<SearchIndexJob>,
) {
    let debounce = std::time::Duration::from_millis(SEARCH_INDEX_DEBOUNCE_MS);
    let mut pending: HashMap<SessionSearchKey, Instant> = HashMap::new();

    loop {
        if pending.is_empty() {
            let Some(job) = rx.recv().await else { break };
            handle_job(root_dir, source, context, &mut pending, job, debounce).await;
            continue;
        }

        let next_deadline = pending
            .values()
            .copied()
            .min()
            .unwrap_or_else(|| Instant::now() + debounce);

        tokio::select! {
            maybe_job = rx.recv() => {
                let Some(job) = maybe_job else { break };
                handle_job(root_dir, source, context, &mut pending, job, debounce).await;
            }
            _ = tokio::time::sleep_until(next_deadline) => {
                flush_ready(root_dir, source, context, &mut pending).await;
            }
        }
    }
}

static BOOTSTRAP_FAIL_LOG: HealAwareLogCounter = HealAwareLogCounter::new(4);

async fn handle_job(
    root_dir: &Path,
    source: &dyn SessionSource,
    context: &WorkerContext,
    pending: &mut HashMap<SessionSearchKey, Instant>,
    job: SearchIndexJob,
    debounce: std::time::Duration,
) {
    match job {
        SearchIndexJob::Upsert(key) => {
            pending.insert(key, Instant::now() + debounce);
        }
        SearchIndexJob::BootstrapAll => {
            let _bootstrapping = BootstrappingGuard::new(&context.progress);
            match bootstrap_with_lease(root_dir, source, context.extract, &context.progress).await {
                Ok(BootstrapOutcome::Done) => {}
                Ok(BootstrapOutcome::RunAgain) => {
                    context.bootstrap_once(root_dir.to_path_buf());
                }
                Err(e) => BOOTSTRAP_FAIL_LOG.warn(
                    "bootstrap failures",
                    "session search bootstrap failed",
                    None,
                    Some(&e),
                ),
            }
            context.refresh_substrings(root_dir);
        }
        SearchIndexJob::RecheckBootstrap => {
            let _bootstrapping = BootstrappingGuard::new(&context.progress);
            match has_completed_bootstrap_marker(root_dir).await {
                Some(true) => {}
                Some(false) => {
                    tracing::info!(
                        "session search index missing completed-bootstrap marker; entering bootstrap gate"
                    );
                    match try_bootstrap_with_lease(
                        root_dir,
                        source,
                        context.extract,
                        &context.progress,
                    )
                    .await
                    {
                        Ok(BootstrapOutcome::Done) => {}
                        Ok(BootstrapOutcome::RunAgain) => {
                            context.bootstrap_once(root_dir.to_path_buf());
                        }
                        Err(e) => BOOTSTRAP_FAIL_LOG.warn(
                            "bootstrap failures",
                            "session search re-bootstrap failed",
                            None,
                            Some(&e),
                        ),
                    }
                }
                // Transient read failure: rebuilding on every one would be a
                // reindex storm; the next search retries the probe.
                None => {
                    tracing::debug!(
                        "session search bootstrap marker unreadable; skipping re-bootstrap"
                    );
                }
            }
            context.refresh_substrings(root_dir);
        }
        SearchIndexJob::RefreshSubstrings => {
            match with_search_index_blocking(&search_db_path(root_dir), |index| {
                index.refresh_substring_index()
            })
            .await
            {
                Ok(true) => context.refresh_substrings(root_dir),
                Ok(false) => {}
                Err(error) => BOOTSTRAP_FAIL_LOG.warn(
                    "substring index refresh failures",
                    "session substring index refresh failed",
                    None,
                    Some(&error),
                ),
            }
        }
    }
}

async fn flush_ready(
    root_dir: &Path,
    source: &dyn SessionSource,
    context: &WorkerContext,
    pending: &mut HashMap<SessionSearchKey, Instant>,
) {
    let now = Instant::now();
    let ready: Vec<SessionSearchKey> = pending
        .iter()
        .filter_map(|(key, deadline)| (*deadline <= now).then_some(key.clone()))
        .collect();

    for key in ready {
        pending.remove(&key);
        if let Err(e) = upsert_by_key(root_dir, source, context, &key).await {
            log_session_index_failure(
                &key.session_id,
                &e,
                "failed upserting session in search index",
            );
        }
    }
}

async fn upsert_by_key(
    root_dir: &Path,
    source: &dyn SessionSource,
    context: &WorkerContext,
    key: &SessionSearchKey,
) -> io::Result<()> {
    // `None` is a deleted session; a read failure surfaces as `Err` and
    // leaves the existing index row alone.
    match source.load_session(&key.session_id, &key.cwd).await? {
        Some(session) => upsert_session(root_dir, &session, context.extract)
            .await
            .map(|_| ()),
        None => delete_session(root_dir, &key.session_id).await,
    }
}

async fn upsert_session(
    root_dir: &Path,
    session: &IndexableSession,
    extract: ContentExtractor,
) -> io::Result<UpsertOutcome> {
    let (content, bytes_read) = if let Some(updates_path) = session.updates_path.clone() {
        tokio::task::spawn_blocking(move || extract(&updates_path))
            .await
            .map_err(io::Error::other)??
    } else {
        return Ok(UpsertOutcome::NoContent);
    };
    let doc = build_session_doc(session, content);
    let db_path = search_db_path(root_dir);

    tokio::task::spawn_blocking(move || {
        with_search_index(&db_path, |index| {
            upsert_unless_unchanged(index, &doc, bytes_read)
        })
    })
    .await
    .map_err(io::Error::other)?
}

async fn delete_session(root_dir: &Path, session_id: &str) -> io::Result<()> {
    let db_path = search_db_path(root_dir);
    let session_id = session_id.to_string();
    tokio::task::spawn_blocking(move || {
        with_search_index(&db_path, |index| index.delete_doc(&session_id))
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Permission bits (`mode & 0o777`) of `path`, for owner-only assertions.
    #[cfg(unix)]
    fn unix_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    /// A store with no sessions: enough for every test here, which exercise
    /// the query and bootstrap-flag paths rather than indexing.
    struct EmptySource;

    #[async_trait::async_trait]
    impl SessionSource for EmptySource {
        async fn list_sessions(&self) -> io::Result<Vec<IndexableSession>> {
            Ok(Vec::new())
        }

        async fn load_session(
            &self,
            _session_id: &str,
            _cwd: &str,
        ) -> io::Result<Option<IndexableSession>> {
            Ok(None)
        }
    }

    fn no_content(_path: &Path) -> io::Result<(String, u64)> {
        Ok((String::new(), 0))
    }

    fn session_is_indexed(root: &Path, query: &str) -> bool {
        !with_search_index(&search_db_path(root), |index| {
            index.query(query, None, 10, 0, false)
        })
        .unwrap()
        .results
        .is_empty()
    }

    fn test_manager() -> SearchIndexManager {
        SearchIndexManager::start(
            |_root| -> Box<dyn SessionSource> { Box::new(EmptySource) },
            no_content,
        )
    }

    fn test_session(session_id: &str, cwd: &str, title: &str) -> IndexableSession {
        IndexableSession {
            session_id: session_id.to_string(),
            cwd: cwd.to_string(),
            updated_at_unix: 1_700_000_000,
            title: title.to_string(),
            updates_path: None,
        }
    }

    #[test]
    #[cfg(unix)]
    fn search_db_path_tightens_sessions_root() {
        let tmp = tempfile::TempDir::new().unwrap();

        let _ = search_db_path(tmp.path());

        assert_eq!(
            unix_mode(&tmp.path().join("sessions")),
            0o700,
            "sessions root must be 0700"
        );
    }

    #[tokio::test]
    async fn test_execute_search_empty_query() {
        let tmp = tempfile::TempDir::new().unwrap();
        let manager = test_manager();
        let req = SessionSearchRequest {
            query: "   ".to_string(),
            cwd: None,
            limit: 10,
            offset: 0,
            include_content: false,
        };
        let resp = execute_search(Some(&manager), tmp.path(), &req)
            .await
            .unwrap();
        assert!(resp.results.is_empty());
        assert_eq!(resp.total_estimate, Some(0));
    }

    #[test]
    fn test_execute_search_returns_empty_on_fresh_db() {
        // Test the index directly instead of via `execute_search()` to avoid
        // a race with a manager's bootstrap worker that concurrently opens
        // the same SQLite DB (flaky "database is locked").
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = search_db_path(tmp.path());
        let index = SessionSearchIndex::open_or_create(&db_path).expect("open fresh DB");
        let result = index.query("hello world", None, 10, 0, false).unwrap();
        assert!(result.results.is_empty());
    }

    #[test]
    fn test_bootstrap_progress_extended_defaults() {
        let progress = BootstrapProgress::default();
        assert!(!progress.is_bootstrapping());
        assert_eq!(progress.indexed.load(Ordering::Relaxed), 0);
        assert_eq!(progress.total.load(Ordering::Relaxed), 0);
        assert_eq!(progress.skipped.load(Ordering::Relaxed), 0);
        assert_eq!(progress.unchanged.load(Ordering::Relaxed), 0);
        assert_eq!(progress.bytes_read.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_search_index_status_serialization() {
        let status = SearchIndexStatus {
            bootstrapping: true,
            indexed: 10,
            total: 20,
            skipped: 3,
            unchanged: 5,
        };
        let json = serde_json::to_string(&status).unwrap();
        assert!(json.contains("\"skipped\":3"));
        assert!(json.contains("\"unchanged\":5"));
        assert!(json.contains("\"bootstrapping\":true"));
    }

    // NOTE: the `bootstrapping` flag is per-manager but shared across every
    // root a manager serves, so tests that depend on it transitioning to
    // `false` are racy. Only the eager-set test is reliable, because the
    // store is synchronous before the channel send.

    #[tokio::test]
    async fn test_bootstrap_once_sets_flag_eagerly() {
        let tmp = tempfile::TempDir::new().unwrap();
        let manager = test_manager();
        manager.bootstrap_once(tmp.path().to_path_buf());
        assert!(
            manager.progress.is_bootstrapping(),
            "bootstrapping flag must be true immediately after bootstrap_once()",
        );
    }

    #[tokio::test]
    async fn test_execute_search_completes_on_fresh_db() {
        let tmp = tempfile::TempDir::new().unwrap();
        let manager = test_manager();
        let req = SessionSearchRequest {
            query: "nonexistent-query-xyzzy".to_string(),
            cwd: None,
            limit: 10,
            offset: 0,
            include_content: false,
        };
        let resp = execute_search(Some(&manager), tmp.path(), &req)
            .await
            .unwrap();
        assert!(resp.results.is_empty());
    }

    /// End-to-end recheck healing: `RecheckBootstrap` on a marker-less index
    /// re-runs the full bootstrap, which rewrites the marker on completion.
    #[tokio::test]
    async fn test_recheck_bootstrap_reruns_reindex_when_marker_missing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let source = EmptySource;
        let (tx, _rx) = mpsc::unbounded_channel::<SearchManagerCmd>();
        let context = WorkerContext {
            tx,
            progress: Arc::new(BootstrapProgress::default()),
            source_factory: |_root| -> Box<dyn SessionSource> { Box::new(EmptySource) },
            extract: no_content,
        };
        let mut pending: HashMap<SessionSearchKey, Instant> = HashMap::new();

        assert_eq!(has_completed_bootstrap_marker(root).await, Some(false));
        handle_job(
            root,
            &source,
            &context,
            &mut pending,
            SearchIndexJob::RecheckBootstrap,
            Duration::from_millis(1),
        )
        .await;
        assert_eq!(
            has_completed_bootstrap_marker(root).await,
            Some(true),
            "recheck on a marker-less index must re-run the bootstrap, which rewrites the marker"
        );
    }

    /// Regression shape: a v3-era indexer silently extracted "" for
    /// sessions with JSON escapes but still recorded a content hash, so at
    /// the *same* schema version the hash dedup keeps skipping identical
    /// (buggy) re-extractions forever. Pins that the v4 upgrade drop removes
    /// the stub row and its hash, so the next bootstrap re-indexes from
    /// scratch instead of being blocked by the stale hash.
    #[test]
    fn test_upgrade_drop_clears_stub_docs_and_hashes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = search_db_path(tmp.path());

        let session = test_session("stub", "/ws", "");
        let stub = build_session_doc(&session, String::new());
        {
            let index = SessionSearchIndex::open_or_create(&db_path).unwrap();
            index.upsert_doc(&stub).unwrap();
            // The empty-content stub still records a hash — re-extracting
            // the same (empty) content would dedup to Unchanged.
            assert_eq!(
                index.get_content_hash("stub").unwrap().as_deref(),
                Some(stub.content_hash.as_str())
            );
            index.set_meta(META_KEY_SCHEMA_VERSION, "3").unwrap();
        }

        let index = SessionSearchIndex::open_or_create(&db_path).unwrap();
        assert_eq!(
            index.get_content_hash("stub").unwrap(),
            None,
            "the upgrade drop must clear stub rows so their stale hashes cannot block re-indexing"
        );
    }

    #[tokio::test]
    async fn evict_removes_the_row_and_never_creates_the_index() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();

        evict_session(root, "s1").await;
        assert!(!search_index_exists(root), "no index may be created");

        let doc = crate::doc::build_session_doc(
            &test_session("s1", "/ws", "a memorable title"),
            "indexed body text".to_string(),
        );
        with_search_index(&search_db_path(root), |index| index.upsert_doc(&doc)).unwrap();
        assert!(session_is_indexed(root, "a memorable title"));

        evict_session(root, "s1").await;

        assert!(
            !session_is_indexed(root, "a memorable title"),
            "a delete must take the row with it",
        );
    }

    #[tokio::test]
    async fn background_backfill_finishes_without_another_search() {
        let tmp = tempfile::TempDir::new().unwrap();
        let root = tmp.path();
        let path = search_db_path(root);
        let manager = test_manager();
        let request = SessionSearchRequest {
            query: "验收".into(),
            cwd: None,
            limit: 30,
            offset: 0,
            include_content: false,
        };
        // Let the manager's once-per-launch source bootstrap finish first.
        assert!(
            execute_search(Some(&manager), root, &request)
                .await
                .unwrap()
                .results
                .is_empty()
        );
        let mut older_writer = rusqlite::Connection::open(&path).unwrap();
        let tx = older_writer.transaction().unwrap();
        for i in 0..24 {
            tx.execute(
                "INSERT INTO session_docs(session_id, cwd, updated_at, title, content, content_hash)
                 VALUES (?1, '/ws', 1, '部署记录', '部署验收完成', '')",
                [i.to_string()],
            ).unwrap();
        }
        tx.commit().unwrap();
        drop(older_writer);
        manager.bootstrap_once(root.to_path_buf());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !with_search_index(&path, |index| index.substring_index_pending()).unwrap() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("worker must schedule all backfill batches");
        let result = execute_search(Some(&manager), root, &request)
            .await
            .unwrap();
        assert_eq!(result.results.len(), 24);
        assert!(!result.bootstrapping);
    }
}
