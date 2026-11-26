//! Corruption self-heal for the session-search SQLite cache: classify an
//! unusable file, then quarantine it under a lock so a fresh empty database
//! can be recreated. The index layer drives the retry.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use echo_agent_sqlite_journal::JournalMode;
use rusqlite::ErrorCode;

static HEAL_LOCK: Mutex<()> = Mutex::new(());

/// Bumped each time the cache is quarantined and recreated, so callers can tell
/// they are now looking at a different incarnation of the on-disk index.
static CACHE_EPOCH: AtomicU64 = AtomicU64::new(0);

pub(crate) fn current_epoch() -> u64 {
    CACHE_EPOCH.load(Ordering::Acquire)
}

/// A snapshot of the [`CACHE_EPOCH`], used to detect whether the cache was
/// quarantined and recreated between two points in this process.
pub(crate) struct CacheEpoch(u64);

impl CacheEpoch {
    pub(crate) fn now() -> Self {
        Self(CACHE_EPOCH.load(Ordering::Acquire))
    }

    pub(crate) fn changed(&self) -> bool {
        CACHE_EPOCH.load(Ordering::Acquire) != self.0
    }
}

pub(crate) fn is_unusable_db_error(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(err, msg) => {
            if matches!(
                err.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ) {
                return true;
            }
            msg.as_deref().is_some_and(message_indicates_unusable_db)
        }
        _ => false,
    }
}

/// Specific phrases only: a bare "malformed" would also match a bad FTS query.
fn message_indicates_unusable_db(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("disk image is malformed")
        || lower.contains("malformed database schema")
        || lower.contains("is not a database")
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

pub(crate) fn quarantine_db_files(db_path: &Path) -> Option<PathBuf> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let corrupt_suffix = format!(".corrupt.{ts}");

    for suffix in ["-wal", "-shm", "-journal"] {
        let side = with_suffix(db_path, suffix);
        if !side.exists() {
            continue;
        }
        let dest = with_suffix(&side, &corrupt_suffix);
        if let Err(e) = std::fs::rename(&side, &dest) {
            tracing::debug!(
                error = %e,
                path = %side.display(),
                "could not quarantine sqlite sidecar; left in place"
            );
        }
    }

    let main_quarantine = with_suffix(db_path, &corrupt_suffix);
    let renamed = if db_path.exists() {
        match std::fs::rename(db_path, &main_quarantine) {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    path = %db_path.display(),
                    "failed to quarantine corrupt session search db; left in place"
                );
                false
            }
        }
    } else {
        false
    };

    renamed.then_some(main_quarantine)
}

pub(crate) fn heal_unusable(
    db_path: &Path,
    cause: &rusqlite::Error,
    reprobe: impl FnOnce(&Path) -> Result<bool, rusqlite::Error>,
    recreate: impl FnOnce(&Path) -> Result<(), rusqlite::Error>,
) {
    use fs2::FileExt as _;

    let _guard = HEAL_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let effective = JournalMode::for_db_path(db_path).effective_db_path(db_path);

    let lock_path = with_suffix(&effective, ".lock");
    let _lock_file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::debug!(error = %e, path = %lock_path.display(), "could not open heal lock; skipping quarantine");
            return;
        }
    };
    if let Err(e) = _lock_file.lock_exclusive() {
        tracing::debug!(error = %e, "could not acquire cross-process heal lock; skipping quarantine");
        return;
    }

    match reprobe(&effective) {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) if is_unusable_db_error(&e) => {}
        Err(_) => return,
    }

    let quarantine = quarantine_db_files(&effective);
    let recreated = recreate(&effective);
    if let Err(e) = &recreated {
        tracing::warn!(error = %e, "failed to recreate session search index after quarantine");
    }

    if quarantine.is_none() && recreated.is_err() {
        tracing::warn!(
            db_path = %effective.display(),
            error = %cause,
            "session search index unusable but could not be quarantined or recreated; left in place"
        );
        return;
    }

    CACHE_EPOCH.fetch_add(1, Ordering::Release);
    tracing::warn!(
        db_path = %effective.display(),
        quarantine = %quarantine
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(removed or missing)".into()),
        error = %cause,
        "session search index unusable; quarantined and recreated empty cache"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn has_corrupt_sibling(dir: &Path) -> bool {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("corrupt"))
    }

    #[test]
    fn quarantine_moves_main_and_sidecars() {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("session_search.sqlite");
        std::fs::write(&db, b"main").unwrap();
        std::fs::write(tmp.path().join("session_search.sqlite-wal"), b"wal").unwrap();
        std::fs::write(tmp.path().join("session_search.sqlite-shm"), b"shm").unwrap();

        let moved = quarantine_db_files(&db).expect("main db should be quarantined");

        assert!(!db.exists());
        assert!(moved.exists());
        assert!(!tmp.path().join("session_search.sqlite-wal").exists());
        assert!(!tmp.path().join("session_search.sqlite-shm").exists());
    }

    fn quarantined_after(reprobe: impl FnOnce(&Path) -> Result<bool, rusqlite::Error>) -> bool {
        let tmp = TempDir::new().unwrap();
        let db = tmp.path().join("session_search.sqlite");
        std::fs::write(&db, b"looks-like-a-db").unwrap();
        heal_unusable(&db, &rusqlite::Error::QueryReturnedNoRows, reprobe, |_| {
            Ok(())
        });
        has_corrupt_sibling(tmp.path())
    }

    #[test]
    fn heal_quarantines_only_on_confirmed_corruption() {
        assert!(!quarantined_after(|_| Ok(true)), "healthy: no quarantine");
        assert!(
            !quarantined_after(|_| Err(rusqlite::Error::QueryReturnedNoRows)),
            "transient failure: no quarantine"
        );
        assert!(quarantined_after(|_| Ok(false)), "corrupt: quarantine");
    }

    #[test]
    fn classifier_ignores_bad_query_but_catches_corruption() {
        assert!(message_indicates_unusable_db(
            "database disk image is malformed"
        ));
        assert!(message_indicates_unusable_db(
            "malformed database schema (members)"
        ));
        assert!(message_indicates_unusable_db("file is not a database"));
        assert!(message_indicates_unusable_db(
            "file is encrypted or is not a database"
        ));
        assert!(!message_indicates_unusable_db("malformed MATCH expression"));
        assert!(!message_indicates_unusable_db("database is corrupt"));
    }
}
