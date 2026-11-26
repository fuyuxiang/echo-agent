//! Index-access plumbing for the session search SQLite cache.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::fts::{self, SessionSearchIndex};
use crate::recovery;

/// The file the index would open, without creating anything. The journal-mode classifier inspects
/// the parent directory, so a caller that means to write must go through [`search_db_path`] first.
fn db_path_in(root_dir: &Path) -> PathBuf {
    let path = root_dir.join("sessions").join("session_search.sqlite");
    echo_agent_sqlite_journal::JournalMode::for_db_path(&path).effective_db_path(&path)
}

/// The same path, with the parent directory created owner only, because the index duplicates
/// session text into the database file. Best effort: the classifier needs the directory to exist.
pub(crate) fn search_db_path(root_dir: &Path) -> PathBuf {
    let _ = echo_agent_config::create_dir_all_owner_only(&root_dir.join("sessions"));
    db_path_in(root_dir)
}

/// Whether an index was built earlier. Creates nothing.
pub(crate) fn search_index_exists(root_dir: &Path) -> bool {
    db_path_in(root_dir).exists()
}

pub(crate) fn sqlite_to_io_error(error: rusqlite::Error) -> io::Error {
    io::Error::other(format!("sqlite error: {error}"))
}

/// Rate-limits a repetitive log site: the first `cap` events go to `warn`,
/// the rest to `debug`; the budget resets when the search cache is healed.
pub(crate) struct HealAwareLogCounter {
    count: AtomicU64,
    epoch_seen: AtomicU64,
    cap: u64,
}

impl HealAwareLogCounter {
    pub(crate) const fn new(cap: u64) -> Self {
        Self {
            count: AtomicU64::new(0),
            epoch_seen: AtomicU64::new(0),
            cap,
        }
    }

    /// Rate-limited warn: within the budget, a tracing warn plus an entry
    /// in the always-on `unified.jsonl`; past it, tracing debug only.
    pub(crate) fn warn(
        &self,
        kind: &str,
        message: &str,
        session_id: Option<&str>,
        error: Option<&io::Error>,
    ) {
        let error_text = error.map(|e| e.to_string());
        if self.should_warn(kind) {
            tracing::warn!(error = error_text.as_deref(), session_id, "{message}");
            echo_agent_telemetry::unified_log::emit(
                echo_agent_telemetry::unified_log::LogLevel::Warn,
                message,
                session_id,
                error_text.map(|e| serde_json::json!({ "error": e })),
            );
        } else {
            tracing::debug!(error = error_text.as_deref(), session_id, "{message}");
        }
    }

    fn should_warn(&self, kind: &str) -> bool {
        let epoch = recovery::current_epoch();
        if self.epoch_seen.swap(epoch, Ordering::Relaxed) != epoch {
            self.count.store(0, Ordering::Relaxed);
        }
        let n = self.count.fetch_add(1, Ordering::Relaxed);
        if n < self.cap {
            return true;
        }
        if n == self.cap {
            tracing::warn!(cap = self.cap, "further {kind} will be logged at debug");
        }
        false
    }
}

static INDEX_FAIL_LOG: HealAwareLogCounter = HealAwareLogCounter::new(8);
static BOOTSTRAP_TIMEOUT_LOG: HealAwareLogCounter = HealAwareLogCounter::new(8);

pub(crate) fn log_session_index_failure(session_id: &str, error: &io::Error, message: &str) {
    INDEX_FAIL_LOG.warn("index failures", message, Some(session_id), Some(error));
}

pub(crate) fn log_bootstrap_timeout(session_id: &str, timeout_secs: u64) {
    let message = format!("session indexing timed out during bootstrap after {timeout_secs}s");
    BOOTSTRAP_TIMEOUT_LOG.warn("bootstrap timeouts", &message, Some(session_id), None);
}

/// Open the index (self-heals unusable files) and run `op`.
pub(crate) fn with_search_index<R>(
    db_path: &Path,
    op: impl Fn(&SessionSearchIndex) -> Result<R, rusqlite::Error>,
) -> io::Result<R> {
    fts::with_index(db_path, op).map_err(sqlite_to_io_error)
}

/// Run `op` on a blocking thread via [`with_search_index`].
pub(crate) async fn with_search_index_blocking<R: Send + 'static>(
    db_path: &Path,
    op: impl Fn(&SessionSearchIndex) -> Result<R, rusqlite::Error> + Send + 'static,
) -> io::Result<R> {
    let path = db_path.to_path_buf();
    tokio::task::spawn_blocking(move || with_search_index(&path, op))
        .await
        .map_err(io::Error::other)?
}
