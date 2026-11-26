//! Cross-process bootstrap lifecycle for the session search index: a lease
//! claim in the index's own `meta` table lets one process run [`reindex_all`]
//! while waiters adopt its completed-bootstrap marker.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::db::{
    HealAwareLogCounter, log_bootstrap_timeout, log_session_index_failure, search_db_path,
    sqlite_to_io_error, with_search_index, with_search_index_blocking,
};
use crate::doc::{UpsertOutcome, build_session_doc, should_skip_session, upsert_unless_unchanged};
use crate::fts::{META_KEY_BOOTSTRAP_CLAIM, META_KEY_LAST_BOOTSTRAP, SessionSearchIndex};
use crate::recovery;
use crate::source::{ContentExtractor, IndexableSession, SessionSource};

const BOOTSTRAP_MAX_CONCURRENT: usize = 4;
const BOOTSTRAP_PER_SESSION_TIMEOUT: Duration = Duration::from_secs(30);
const BOOTSTRAP_MAX_FILE_SIZE: u64 = 30 * 1024 * 1024;

/// Bootstrap coordination timing, injectable so tests run in milliseconds.
struct BootstrapTiming {
    /// Claims stamped longer ago than this are abandoned and may be taken over.
    lease: Duration,
    refresh: Duration,
    peer_wait: Duration,
    poll: Duration,
}

const TIMING: BootstrapTiming = BootstrapTiming {
    lease: Duration::from_secs(300),
    refresh: Duration::from_secs(30),
    peer_wait: Duration::from_secs(60),
    poll: Duration::from_secs(1),
};
// The refresh must fire several times within a lease, and a waiter must
// poll at least once within the peer wait.
// (`try_bootstrap_with_lease` zeroes the peer wait on purpose: one claim
// attempt, no wait loop.)
const _: () = assert!(TIMING.refresh.as_millis() < TIMING.lease.as_millis());
const _: () = assert!(TIMING.poll.as_millis() < TIMING.peer_wait.as_millis());

#[derive(Default)]
pub(crate) struct BootstrapProgress {
    /// Bit 0 is the `bootstrapping` flag; the upper bits count armings.
    /// One atomic, so a finished job can clear the flag with a single
    /// compare-exchange only when nothing newer (a heal re-enqueue, a
    /// concurrent search) armed it.
    state: AtomicU64,
    pub indexed: AtomicU64,
    pub total: AtomicU64,
    /// Sessions skipped due to size limit or timeout.
    pub skipped: AtomicU64,
    pub unchanged: AtomicU64,
    pub bytes_read: AtomicU64,
}

impl BootstrapProgress {
    pub(crate) fn is_bootstrapping(&self) -> bool {
        self.state.load(Ordering::Acquire) & 1 == 1
    }

    /// Set the flag; returns the generation a guard must match to clear it.
    pub(crate) fn begin_bootstrapping(&self) -> u64 {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            let generation = (current >> 1) + 1;
            match self.state.compare_exchange_weak(
                current,
                generation << 1 | 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return generation,
                Err(next) => current = next,
            }
        }
    }

    /// Clear the flag, unless a newer arming owns it.
    fn end_bootstrapping(&self, generation: u64) {
        let _ = self.state.compare_exchange(
            generation << 1 | 1,
            generation << 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
}

/// Holds the `bootstrapping` flag high for the duration of a bootstrap job
/// and clears it on every exit, including unwind, unless a newer set has
/// taken ownership since.
pub(crate) struct BootstrappingGuard {
    progress: Arc<BootstrapProgress>,
    generation: u64,
}

impl BootstrappingGuard {
    pub(crate) fn new(progress: &Arc<BootstrapProgress>) -> Self {
        Self {
            progress: Arc::clone(progress),
            generation: progress.begin_bootstrapping(),
        }
    }
}

impl Drop for BootstrappingGuard {
    fn drop(&mut self) {
        self.progress.end_bootstrapping(self.generation);
    }
}

static CLAIM_LOG: HealAwareLogCounter = HealAwareLogCounter::new(4);

/// Run [`reindex_all`] at most once at a time across concurrent echoagent
/// processes. A launch's first claim always reindexes, even when a completed
/// marker exists; waiters adopt any completed marker, stale ones included
/// (the claimant refreshes the index either way), and give up after the
/// bounded wait.
pub(crate) async fn bootstrap_with_lease(
    root_dir: &Path,
    source: &dyn SessionSource,
    extract: ContentExtractor,
    progress: &Arc<BootstrapProgress>,
) -> io::Result<BootstrapOutcome> {
    bootstrap_with_lease_inner(
        root_dir,
        source,
        extract,
        progress,
        &TIMING,
        BootstrapRole::Launch,
    )
    .await
}

/// Single claim attempt: rebuilds when the lease is free and no completed
/// marker exists, adopts the marker otherwise, and returns at once when a
/// peer holds the lease. Rechecks use this so a rebuild that outlives the
/// peer wait cannot re-block the worker on every later search.
pub(crate) async fn try_bootstrap_with_lease(
    root_dir: &Path,
    source: &dyn SessionSource,
    extract: ContentExtractor,
    progress: &Arc<BootstrapProgress>,
) -> io::Result<BootstrapOutcome> {
    bootstrap_with_lease_inner(
        root_dir,
        source,
        extract,
        progress,
        &TIMING,
        BootstrapRole::Recheck,
    )
    .await
}

/// What the caller owes after the gate returns.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum BootstrapOutcome {
    /// Completed, adopted, or gave up: the caller owes nothing more.
    Done,
    /// The cache healed mid-run, so the index must be bootstrapped again.
    RunAgain,
}

/// How the caller entered the gate; the entry points document what each
/// role owes.
#[derive(Clone, Copy, PartialEq)]
enum BootstrapRole {
    Launch,
    Recheck,
}

async fn bootstrap_with_lease_inner(
    root_dir: &Path,
    source: &dyn SessionSource,
    extract: ContentExtractor,
    progress: &Arc<BootstrapProgress>,
    timing: &BootstrapTiming,
    role: BootstrapRole,
) -> io::Result<BootstrapOutcome> {
    let db_path = search_db_path(root_dir);
    let token = ClaimToken::new();
    let started = Instant::now();
    let peer_wait = match role {
        BootstrapRole::Launch => timing.peer_wait,
        BootstrapRole::Recheck => Duration::ZERO,
    };
    let deadline = started + peer_wait;
    let mut peer_seen = false;
    loop {
        // Skipped on the first iteration so a launch always reindexes.
        if peer_seen && has_completed_bootstrap_marker(root_dir).await == Some(true) {
            tracing::info!(
                waited_ms = started.elapsed().as_millis() as u64,
                "adopted a peer's completed session search bootstrap"
            );
            return Ok(BootstrapOutcome::Done);
        }

        if claim_bootstrap_lease(&db_path, &token, timing.lease).await? {
            // Only a launch's first claim ignores an existing marker (the
            // launch owes pruning and skipped retries); everyone else
            // adopts any completed marker.
            let first_launch_claim = role != BootstrapRole::Recheck && !peer_seen;
            if !first_launch_claim && has_completed_bootstrap_marker(root_dir).await == Some(true) {
                release_bootstrap_claim(&db_path, &token).await;
                tracing::info!(
                    waited_ms = started.elapsed().as_millis() as u64,
                    "adopted a peer's completed session search bootstrap"
                );
                return Ok(BootstrapOutcome::Done);
            }
            tracing::info!(
                token = %token,
                contended = peer_seen,
                waited_ms = started.elapsed().as_millis() as u64,
                "claimed session search bootstrap lease"
            );
            let refresher = spawn_claim_refresher(db_path.clone(), token.clone(), timing.refresh);
            let result = reindex_all(
                root_dir,
                source,
                extract,
                progress,
                &token,
                refresher.claim_lost(),
            )
            .await;
            drop(refresher);
            release_bootstrap_claim(&db_path, &token).await;
            return result;
        }
        peer_seen = true;

        if Instant::now() >= deadline {
            // Debug on the recheck path, which runs per search while a
            // peer rebuilds.
            if role == BootstrapRole::Recheck {
                tracing::debug!("peer process is bootstrapping the shared session search index");
            } else {
                tracing::info!(
                    "peer process is bootstrapping the shared session search index; not waiting"
                );
            }
            return Ok(BootstrapOutcome::Done);
        }
        tokio::time::sleep(timing.poll).await;
    }
}

/// Owner token that fences every claim-scoped write to the shared index.
#[derive(Clone)]
struct ClaimToken(String);

impl ClaimToken {
    fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ClaimToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Returns `true` when this process claimed the bootstrap lease.
async fn claim_bootstrap_lease(
    db_path: &Path,
    token: &ClaimToken,
    lease: Duration,
) -> io::Result<bool> {
    let token = token.as_str().to_string();
    with_search_index_blocking(db_path, move |index| {
        index.try_claim_bootstrap(chrono::Utc::now().timestamp(), lease, &token)
    })
    .await
}

/// Aborts the refresher on drop so no detached task outlives the gate.
struct RefresherGuard {
    handle: tokio::task::JoinHandle<()>,
    claim_lost: Arc<AtomicBool>,
}

impl RefresherGuard {
    /// Latched when the refresher sees the claim held by someone else.
    fn claim_lost(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.claim_lost)
    }
}

impl Drop for RefresherGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn spawn_claim_refresher(db_path: PathBuf, token: ClaimToken, every: Duration) -> RefresherGuard {
    let claim_lost = Arc::new(AtomicBool::new(false));
    let lost = Arc::clone(&claim_lost);
    let handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(every);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The first tick fires immediately; the claim was stamped just now.
        interval.tick().await;
        loop {
            interval.tick().await;
            let refreshed = with_search_index_blocking(&db_path, {
                let token = token.as_str().to_string();
                move |index| index.refresh_bootstrap_claim(chrono::Utc::now().timestamp(), &token)
            })
            .await;
            match refreshed {
                Ok(true) => {}
                Ok(false) => {
                    lost.store(true, Ordering::Release);
                    CLAIM_LOG.warn(
                        "bootstrap claim losses",
                        "bootstrap claim lost mid-reindex; a peer took over",
                        None,
                        None,
                    );
                    return;
                }
                Err(e) => {
                    CLAIM_LOG.warn(
                        "bootstrap claim refresh failures",
                        "failed to refresh bootstrap claim lease",
                        None,
                        Some(&e),
                    );
                }
            }
        }
    });
    RefresherGuard { handle, claim_lost }
}

/// Best-effort; on any failure the lease expiry is the fallback.
async fn release_bootstrap_claim(db_path: &Path, token: &ClaimToken) {
    let token = token.as_str().to_string();
    let released =
        with_search_index_blocking(db_path, move |index| index.release_bootstrap_claim(&token))
            .await;
    match released {
        Ok(true) => {}
        Ok(false) => tracing::debug!("bootstrap claim was already released or taken over"),
        Err(e) => {
            tracing::debug!(error = %e, "failed to release bootstrap claim; lease will expire");
        }
    }
}

/// `Some(true)` marker present, `Some(false)` genuinely absent (bootstrap
/// needed), `None` transient read failure, which must not be mistaken for
/// absence.
pub(crate) async fn has_completed_bootstrap_marker(root_dir: &Path) -> Option<bool> {
    let db_path = search_db_path(root_dir);
    with_search_index_blocking(&db_path, |index| {
        index
            .get_meta(META_KEY_LAST_BOOTSTRAP)
            .map(|marker| marker.is_some())
    })
    .await
    .ok()
}

/// Preserves read failures so "absent" and "could not read" stay distinct.
#[cfg(test)]
pub(crate) fn try_read_last_bootstrap_at(db_path: &Path) -> io::Result<Option<i64>> {
    if !db_path.exists() {
        return Ok(None);
    }
    let index = SessionSearchIndex::open_or_create(db_path).map_err(sqlite_to_io_error)?;
    let value = index
        .get_meta(META_KEY_LAST_BOOTSTRAP)
        .map_err(sqlite_to_io_error)?;
    Ok(value.and_then(|v| v.parse::<i64>().ok()))
}

/// Fenced marker write: returns `false` (no write) when the claim under
/// `token` was lost, so a stale claimant never asserts completion.
fn write_last_bootstrap_at_if_claim_owner(db_path: &Path, token: &str) -> io::Result<bool> {
    let now = chrono::Utc::now().timestamp();
    with_search_index(db_path, |index| {
        index.set_meta_if_claim_owner(META_KEY_LAST_BOOTSTRAP, &now.to_string(), token)
    })
}

/// Whether any process currently holds the bootstrap claim.
fn has_bootstrap_claim(db_path: &Path) -> io::Result<bool> {
    with_search_index(db_path, |index| {
        index
            .get_meta(META_KEY_BOOTSTRAP_CLAIM)
            .map(|claim| claim.is_some())
    })
}

fn clear_last_bootstrap_at(db_path: &Path) -> io::Result<()> {
    with_search_index(db_path, |index| index.delete_meta(META_KEY_LAST_BOOTSTRAP))
}

/// One shared connection per reindex instead of an open per session;
/// re-opens when the cache epoch changes (a heal renames the DB file), falls
/// back to the healing open on unusable-DB errors. A peer's heal is invisible
/// to the local epoch; the fenced marker write covers that case.
struct SharedIndex(parking_lot::Mutex<Option<(u64, SessionSearchIndex)>>);

impl SharedIndex {
    fn new() -> Arc<Self> {
        Arc::new(Self(parking_lot::Mutex::new(None)))
    }

    /// Blocking; call from `spawn_blocking`, like every open in this module.
    fn with<R>(
        &self,
        db_path: &Path,
        op: impl Fn(&SessionSearchIndex) -> Result<R, rusqlite::Error>,
    ) -> io::Result<R> {
        let mut slot = self.0.lock();
        let epoch = recovery::current_epoch();
        if !matches!(&*slot, Some((e, _)) if *e == epoch) {
            let index = SessionSearchIndex::open_or_create(db_path).map_err(sqlite_to_io_error)?;
            *slot = Some((epoch, index));
        }
        let Some((_, index)) = &*slot else {
            unreachable!("slot populated above")
        };
        match op(index) {
            Ok(value) => Ok(value),
            Err(e) if recovery::is_unusable_db_error(&e) => {
                // Drop our fd before healing so quarantine renames cleanly.
                *slot = None;
                drop(slot);
                with_search_index(db_path, op)
            }
            Err(e) => Err(sqlite_to_io_error(e)),
        }
    }
}

async fn reindex_all(
    root_dir: &Path,
    source: &dyn SessionSource,
    extract: ContentExtractor,
    progress: &Arc<BootstrapProgress>,
    claim_token: &ClaimToken,
    claim_lost: Arc<AtomicBool>,
) -> io::Result<BootstrapOutcome> {
    let epoch = recovery::CacheEpoch::now();

    progress.indexed.store(0, Ordering::Relaxed);
    progress.skipped.store(0, Ordering::Relaxed);
    progress.unchanged.store(0, Ordering::Relaxed);
    progress.bytes_read.store(0, Ordering::Relaxed);

    let start = Instant::now();
    // The SessionSource reference cannot be shared across tasks, so each row
    // carries its own transcript path taken during enumeration.
    let sessions: Vec<IndexableSession> = source.list_sessions().await?;
    progress
        .total
        .store(sessions.len() as u64, Ordering::Relaxed);
    let expected_ids: HashSet<String> = sessions.iter().map(|s| s.session_id.clone()).collect();

    let mut skipped_large = 0u64;
    for session in &sessions {
        if let Some(updates_path) = &session.updates_path
            && should_skip_session(updates_path, BOOTSTRAP_MAX_FILE_SIZE)
        {
            skipped_large += 1;
        }
    }

    tracing::info!(
        total_sessions = sessions.len(),
        skipped_large = skipped_large,
        "session search bootstrap starting"
    );

    let semaphore = Arc::new(Semaphore::new(BOOTSTRAP_MAX_CONCURRENT.max(1)));
    let root = root_dir.to_path_buf();
    let shared = SharedIndex::new();

    let mut join_set = tokio::task::JoinSet::new();

    for session in sessions {
        let semaphore = semaphore.clone();
        let progress = progress.clone();
        let root = root.clone();
        let shared = shared.clone();
        let claim_lost = Arc::clone(&claim_lost);
        let per_session_timeout = BOOTSTRAP_PER_SESSION_TIMEOUT;
        let max_file_size = BOOTSTRAP_MAX_FILE_SIZE;

        join_set.spawn(async move {
            let _permit = semaphore
                .acquire()
                .await
                .expect("semaphore is never closed");

            // A successor owns the index once the claim is lost. These upserts are idempotent,
            // not fenced, so stopping just avoids contending with it.
            if claim_lost.load(Ordering::Acquire) {
                return;
            }

            let session_id = session.session_id.clone();
            let updates_path = session.updates_path.clone();

            if let Some(ref path) = updates_path
                && should_skip_session(path, max_file_size)
            {
                let file_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
                tracing::debug!(
                    session_id = %session_id,
                    file_size = file_size,
                    max_size = max_file_size,
                    "skipping large session during bootstrap"
                );
                // Insert a title-only placeholder so title search still works;
                // insert-if-absent so an existing (fuller) row is never touched.
                let doc = build_session_doc(&session, String::new());
                let db_path = search_db_path(&root);
                let shared = shared.clone();
                let title_only = tokio::task::spawn_blocking(move || {
                    shared.with(&db_path, |index| index.insert_doc_if_absent(&doc))
                })
                .await;
                if let Err(e) = title_only.map_err(io::Error::other).and_then(|r| r) {
                    log_session_index_failure(
                        &session_id,
                        &e,
                        "failed to write title-only index row for large session",
                    );
                }
                progress.skipped.fetch_add(1, Ordering::Relaxed);
                return;
            }

            match tokio::time::timeout(per_session_timeout, async move {
                let (content, bytes_read) = if let Some(path) = updates_path {
                    match tokio::task::spawn_blocking(move || extract(&path)).await {
                        Ok(Ok(result)) => result,
                        Ok(Err(e)) => return Err(e),
                        Err(e) => return Err(io::Error::other(e)),
                    }
                } else {
                    return Ok(UpsertOutcome::NoContent);
                };

                let doc = build_session_doc(&session, content);
                let db_path = search_db_path(&root);

                match tokio::task::spawn_blocking(move || {
                    shared.with(&db_path, |index| {
                        upsert_unless_unchanged(index, &doc, bytes_read)
                    })
                })
                .await
                {
                    Ok(result) => result,
                    Err(e) => Err(io::Error::other(e)),
                }
            })
            .await
            {
                Ok(Ok(outcome)) => match outcome {
                    UpsertOutcome::Indexed { bytes_read } => {
                        progress.bytes_read.fetch_add(bytes_read, Ordering::Relaxed);
                    }
                    UpsertOutcome::Unchanged { bytes_read } => {
                        progress.unchanged.fetch_add(1, Ordering::Relaxed);
                        progress.bytes_read.fetch_add(bytes_read, Ordering::Relaxed);
                    }
                    UpsertOutcome::NoContent => {}
                },
                Ok(Err(e)) => {
                    log_session_index_failure(
                        &session_id,
                        &e,
                        "failed to index session for search",
                    );
                    progress.skipped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                Err(_) => {
                    // The abandoned spawn_blocking task runs to completion.
                    log_bootstrap_timeout(&session_id, per_session_timeout.as_secs());
                    progress.skipped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            }
            progress.indexed.fetch_add(1, Ordering::Relaxed);
        });
    }

    while let Some(result) = join_set.join_next().await {
        if let Err(e) = result {
            tracing::warn!(error = %e, "session indexing task panicked");
        }
    }

    if claim_lost.load(Ordering::Acquire) {
        tracing::warn!("bootstrap claim lost; abandoning reindex without a completion marker");
        // A local heal quarantines the claim row with the file, which the
        // fenced refresh cannot tell from a takeover; only the takeover has
        // a successor that finishes the job.
        return Ok(if epoch.changed() {
            BootstrapOutcome::RunAgain
        } else {
            BootstrapOutcome::Done
        });
    }

    // Prune sessions deleted on disk. Fenced: `expected_ids` is a startup
    // snapshot, so a claimant that lost its lease must not delete rows a
    // successor indexed since; the refresh doubles as the ownership check.
    let db_path = search_db_path(root_dir);
    let token = claim_token.as_str().to_string();
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        shared.with(&db_path, |index| {
            if !index.prune_missing_if_claim_owner(
                chrono::Utc::now().timestamp(),
                &token,
                &expected_ids,
            )? {
                tracing::warn!("bootstrap claim lost; skipping stale orphan prune");
            }
            Ok(())
        })
    })
    .await
    .map_err(io::Error::other)??;

    let elapsed = start.elapsed();
    tracing::info!(
        indexed = progress.indexed.load(Ordering::Relaxed),
        skipped = progress.skipped.load(Ordering::Relaxed),
        unchanged = progress.unchanged.load(Ordering::Relaxed),
        duration_ms = elapsed.as_millis() as u64,
        bytes_read = progress.bytes_read.load(Ordering::Relaxed),
        "session search bootstrap complete"
    );

    let db_path = search_db_path(root_dir);
    let mut needs_rebootstrap = epoch.changed();
    if needs_rebootstrap {
        tracing::warn!("session search cache healed during bootstrap; completion marker withheld");
    } else {
        match write_last_bootstrap_at_if_claim_owner(&db_path, claim_token.as_str()) {
            Ok(true) if epoch.changed() => {
                tracing::warn!(
                    "session search cache healed while writing completion marker; clearing it"
                );
                if let Err(e) = clear_last_bootstrap_at(&db_path) {
                    tracing::warn!(error = %e, "failed to clear stale completion marker after heal");
                }
                needs_rebootstrap = true;
            }
            Ok(true) => {}
            // No claim at all means the file was replaced under us (a heal
            // here or in a peer), not taken over; the fresh index is empty
            // and needs a rebuild.
            Ok(false) => match has_bootstrap_claim(&db_path) {
                Ok(false) => {
                    tracing::warn!(
                        "session search index was replaced during bootstrap; rebuilding"
                    );
                    needs_rebootstrap = true;
                }
                _ => tracing::warn!("bootstrap claim lost; completion marker withheld"),
            },
            Err(e) => tracing::warn!(error = %e, "failed to write last_bootstrap_at metadata"),
        }
    }

    if needs_rebootstrap {
        return Ok(BootstrapOutcome::RunAgain);
    }

    Ok(BootstrapOutcome::Done)
}

// Gate tests inject a fresh BootstrapProgress and assert only on their own
// per-tmpdir database state.
#[cfg(test)]
#[path = "bootstrap_tests.rs"]
mod tests;
