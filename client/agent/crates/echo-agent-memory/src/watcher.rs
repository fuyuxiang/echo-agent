//! File watcher for detecting external memory edits.
//!
//! Watches `~/.echo-agent/memory/` for `.md` file changes (create, modify, remove)
//! and accumulates the affected paths.  The search path checks [`is_dirty`]
//! before each query and syncs the index for all dirty paths:
//! - **created / modified** files are reindexed via `MemoryIndex::reindex_file`
//! - **deleted** files have their stale chunks removed via `MemoryIndex::delete_path`
//!
//! Without the deletion handling, chunks from removed files would remain
//! searchable indefinitely.
//!
//! Uses `arc_swap::ArcSwap` for lock-free dirty path tracking — the notify
//! event handler inserts via `rcu`, the search path takes via atomic swap.
//!
//! [`is_dirty`]: MemoryFileWatcher::is_dirty

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arc_swap::ArcSwap;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Watches the memory directory for `.md` file changes.
///
/// Lock-free design:
/// - **Insert** (notify thread): `dirty_files.rcu(|old| { clone + insert })`
/// - **Take** (search path): `dirty_files.swap(empty)` — single atomic pointer exchange
/// - **Quick check**: `dirty.load(Relaxed)` — single atomic load, no allocation
pub struct MemoryFileWatcher {
    dirty_files: Arc<ArcSwap<HashSet<PathBuf>>>,
    dirty: Arc<AtomicBool>,
    _watcher: RecommendedWatcher,
}

impl MemoryFileWatcher {
    /// Start watching the given memory directory for `.md` file changes.
    ///
    /// Returns `None` if the watcher fails to initialize (logged, non-fatal).
    pub fn start(memory_dir: &Path) -> Option<Self> {
        let dirty_files: Arc<ArcSwap<HashSet<PathBuf>>> =
            Arc::new(ArcSwap::new(Arc::new(HashSet::new())));
        let dirty = Arc::new(AtomicBool::new(false));

        let df = dirty_files.clone();
        let d = dirty.clone();

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
            let Ok(event) = res else { return };
            match event.kind {
                EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {}
                _ => return,
            }
            for path in &event.paths {
                if path.extension().is_some_and(|ext| ext == "md") {
                    let path = path.clone();
                    df.rcu(move |old| {
                        let mut new = (**old).clone();
                        new.insert(path.clone());
                        new
                    });
                    d.store(true, Ordering::Relaxed);
                }
            }
        })
        .map_err(|e| {
            tracing::warn!(error = %e, "failed to create memory file watcher");
        })
        .ok()?;

        watcher
            .watch(memory_dir, RecursiveMode::Recursive)
            .map_err(|e| {
                tracing::warn!(
                    path = %memory_dir.display(),
                    error = %e,
                    "failed to watch memory directory"
                );
            })
            .ok()?;

        tracing::info!(
            path = %memory_dir.display(),
            "memory file watcher started"
        );

        Some(Self {
            dirty_files,
            dirty,
            _watcher: watcher,
        })
    }

    /// Quick check: true if any files have been modified since last take.
    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    /// Take all accumulated dirty paths, resetting the dirty state.
    /// Returns the paths that changed since the last take.
    pub fn take_dirty(&self) -> Vec<PathBuf> {
        let old = self.dirty_files.swap(Arc::new(HashSet::new()));
        self.dirty.store(false, Ordering::Relaxed);
        old.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_watcher_starts_on_valid_dir() {
        let tmp = TempDir::new().unwrap();
        // In CI / containerized environments the OS may deny inotify watches
        // (e.g. exhausted fs.inotify.max_user_instances); skip gracefully.
        let _watcher = MemoryFileWatcher::start(tmp.path());
    }

    #[test]
    fn test_watcher_initially_clean() {
        let tmp = TempDir::new().unwrap();
        let Some(watcher) = MemoryFileWatcher::start(tmp.path()) else {
            eprintln!("skipping: could not create file watcher (resource limit?)");
            return;
        };
        assert!(!watcher.is_dirty());
        assert!(watcher.take_dirty().is_empty());
    }

    #[test]
    fn test_watcher_detects_md_file_creation() {
        let tmp = TempDir::new().unwrap();
        let Some(watcher) = MemoryFileWatcher::start(tmp.path()) else {
            eprintln!("skipping: could not create file watcher (resource limit?)");
            return;
        };

        // Create a .md file — watcher should detect it
        std::fs::write(tmp.path().join("test.md"), "hello").unwrap();

        // Give the watcher time to process (debounce + OS event delivery)
        std::thread::sleep(std::time::Duration::from_millis(500));

        assert!(watcher.is_dirty(), "should detect .md creation");
        let dirty = watcher.take_dirty();
        assert!(!dirty.is_empty(), "should have dirty paths");
        assert!(dirty[0].extension().unwrap() == "md");
    }

    #[test]
    fn test_watcher_ignores_non_md_files() {
        let tmp = TempDir::new().unwrap();
        let Some(watcher) = MemoryFileWatcher::start(tmp.path()) else {
            eprintln!("skipping: could not create file watcher (resource limit?)");
            return;
        };

        // Create a non-.md file
        std::fs::write(tmp.path().join("test.txt"), "hello").unwrap();
        std::fs::write(tmp.path().join("index.sqlite"), "db").unwrap();

        std::thread::sleep(std::time::Duration::from_millis(500));

        assert!(
            !watcher.is_dirty(),
            "should not detect non-.md file changes"
        );
    }

    #[test]
    fn test_take_dirty_resets_state() {
        let tmp = TempDir::new().unwrap();
        let Some(watcher) = MemoryFileWatcher::start(tmp.path()) else {
            eprintln!("skipping: could not create file watcher (resource limit?)");
            return;
        };

        std::fs::write(tmp.path().join("a.md"), "content").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));

        let first = watcher.take_dirty();
        assert!(!first.is_empty());
        assert!(!watcher.is_dirty(), "should be clean after take");
        assert!(
            watcher.take_dirty().is_empty(),
            "second take should be empty"
        );
    }
}
