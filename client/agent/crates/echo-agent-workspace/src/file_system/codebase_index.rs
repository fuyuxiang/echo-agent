//! Codebase Index Manager
//!
//! Manages code graph indexes for code navigation features (go-to-definition, go-to-references).
//! Indexes are shared across sessions with the same cwd to avoid duplicate work.
//!
//! ## Deduplication
//!
//! Deduplication happens at two levels:
//! 1. **Process-level**: `IndexManager::spawn()` ensures at most one manager per workspace per process
//! 2. **Cross-process**: File-based locking prevents duplicate background operations

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use echo_agent_codebase_graph::{IndexManager, IndexManagerConfig, IndexManagerHandle};

use echo_agent_tools::util::echo_agent_home::echo_agent_home;

/// Get the cache path for a cwd's index.
///
/// Cache is stored in: `~/.echo-agent/indexes/{url_encoded_cwd}/goto_index.bin`
pub fn get_index_cache_path(cwd: &Path) -> PathBuf {
    let encoded = urlencoding::encode(&cwd.to_string_lossy()).into_owned();
    echo_agent_home()
        .join("indexes")
        .join(encoded)
        .join("goto_index.bin")
}

/// Manages code graph indexes across sessions.
///
/// Wraps `IndexManager::spawn()` with cache-path config and cross-session
/// handle reuse. Keeps only `Weak` refs — sessions hold the strong `Arc`s,
/// so the actor is reaped when the last session in a git-root closes.
pub struct CodebaseIndexManager {
    indexes: HashMap<PathBuf, Weak<IndexManagerHandle>>,
}

impl Default for CodebaseIndexManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CodebaseIndexManager {
    pub fn new() -> Self {
        Self {
            indexes: HashMap::new(),
        }
    }

    /// Get or create the index for `cwd`. Returns `(handle, was_newly_spawned)`.
    /// Caller must hold the `Arc` to keep the index alive.
    pub fn get_or_create(&mut self, cwd: PathBuf) -> (Arc<IndexManagerHandle>, bool) {
        self.indexes.retain(|_, weak| weak.strong_count() > 0);

        if let Some(handle) = self.indexes.get(&cwd).and_then(Weak::upgrade) {
            tracing::info!(
                cwd = %cwd.display(),
                event = "index_reused",
                "Codebase index already running — reusing shared handle"
            );
            return (handle, false);
        }

        // Create cache directory if needed
        let cache_path = get_index_cache_path(&cwd);
        if let Some(parent) = cache_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::warn!(
                path = %parent.display(),
                error = %e,
                "Failed to create cache directory"
            );
        }

        tracing::info!(
            cwd = %cwd.display(),
            cache_path = %cache_path.display(),
            event = "index_lazy_started",
            "Codebase index lazy-started — spawning IndexManager actor"
        );

        // IndexManager::spawn() handles global deduplication.
        let config = IndexManagerConfig::new(cwd.clone()).with_cache_path(cache_path);
        let handle = IndexManager::spawn(config);

        self.indexes.insert(cwd, Arc::downgrade(&handle));
        (handle, true)
    }

    /// Get the running index for `cwd`, or `None` if not started / already reaped.
    pub fn get(&self, cwd: &Path) -> Option<Arc<IndexManagerHandle>> {
        self.indexes.get(cwd).and_then(Weak::upgrade)
    }

    /// Index whose root is the longest prefix of `path` (multi-repo dispatch).
    ///
    /// Upgrades first, then picks the longest *live* covering root: if the
    /// longest-prefix root's `Weak` is already dead, a shorter but still-live
    /// covering root is used instead of returning `None`.
    pub fn get_covering(&self, path: &Path) -> Option<Arc<IndexManagerHandle>> {
        self.indexes
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .filter_map(|(root, weak)| weak.upgrade().map(|handle| (root, handle)))
            .max_by_key(|(root, _)| root.as_os_str().len())
            .map(|(_, handle)| handle)
    }

    /// Ensure an index exists for every materialized mount.
    pub fn ensure_all(&mut self, roots: &[PathBuf]) -> Vec<Arc<IndexManagerHandle>> {
        roots
            .iter()
            .map(|root| self.get_or_create(root.clone()).0)
            .collect()
    }

    /// Returns the number of currently-live indexes (test helper).
    #[cfg(test)]
    pub(crate) fn active_count(&self) -> usize {
        self.indexes
            .values()
            .filter(|weak| weak.strong_count() > 0)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_path_encoding() {
        let cwd = Path::new("/Users/test/my project");
        let cache_path = get_index_cache_path(cwd);

        // Should contain URL-encoded path
        assert!(cache_path.to_string_lossy().contains("%2F"));
        assert!(cache_path.to_string_lossy().ends_with("goto_index.bin"));
    }

    // =========================================================================
    // Lazy-start mechanic tests
    //
    // These tests verify the core lazy-start behavior:
    // `CodebaseIndexManager::get()` returns None before the index is created,
    // which maps to `echo.agent/code/status` reporting `reason: notStarted`.
    // `get_or_create()` is the lazy-start entry point called by
    // `MvpAgent::start_codebase_index_for_code_nav` on the first code-nav
    // request for an eligible session.
    // =========================================================================

    /// An empty CodebaseIndexManager returns None for any path.
    ///
    /// This is the steady-state before ANY code-nav request has been made.
    /// In `echo.agent/code/status`, `resolve_index_handle()` calls
    /// `agent.get_codebase_index(cwd)` which calls `mgr.get(cwd)`.
    /// When this returns None the status reports `reason: notStarted` — the
    /// key non-starting guarantee from the plan.
    #[test]
    fn test_get_returns_none_before_any_index_created() {
        let mgr = CodebaseIndexManager::new();
        assert!(
            mgr.get(Path::new("/some/repo")).is_none(),
            "get() must return None before get_or_create() is called — this is \
             the CodebaseIndexManager state that causes code/status to report notStarted"
        );
    }

    /// `get_covering` must skip a dead longest-prefix `Weak` and fall back to a
    /// shorter but still-live covering root (regression: previously it picked
    /// the longest prefix first and returned `None` if that one was dead).
    #[test]
    fn get_covering_skips_dead_weak_for_shorter_live_root() {
        let temp = tempfile::tempdir().unwrap();
        let outer = temp.path().join("ws");
        let inner = outer.join("app");
        std::fs::create_dir_all(&inner).unwrap();

        let mut mgr = CodebaseIndexManager::new();
        let outer_handle = mgr.get_or_create(outer.clone()).0;
        let inner_handle = mgr.get_or_create(inner.clone()).0;

        let file = inner.join("src/main.rs");
        // While both are live, the longest prefix (`inner`) wins.
        assert!(std::sync::Arc::ptr_eq(
            &mgr.get_covering(&file).expect("live inner"),
            &inner_handle
        ));

        // Drop the longest root's strong ref: its `Weak` is now dead. Covering
        // must fall back to the shorter live `outer`, not return `None`.
        drop(inner_handle);
        let covering = mgr.get_covering(&file).expect("fall back to live outer");
        assert!(std::sync::Arc::ptr_eq(&covering, &outer_handle));

        drop(outer_handle);
    }

    /// get() returns None even after another path was indexed.
    ///
    /// Proves path-scoped isolation: a different cwd's index does not
    /// satisfy a lookup for an unrelated path.
    #[test]
    fn test_get_returns_none_for_different_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let root_a = temp.path().join("repo_a");
        let root_b = temp.path().join("repo_b");
        std::fs::create_dir_all(&root_a).unwrap();

        let mut mgr = CodebaseIndexManager::new();
        let (_handle, _) = mgr.get_or_create(root_a.clone());

        // root_b was never indexed — must return None.
        assert!(
            mgr.get(&root_b).is_none(),
            "get() for an un-indexed path must return None (path-scoped isolation)"
        );
    }

    /// After get_or_create() the same path is found by get().
    ///
    /// This is the lazy-start path: `get_or_create` is called by
    /// `start_codebase_index_for_code_nav` on the first eligible code-nav
    /// request, after which `get_codebase_index` (used by `resolve_index_handle`
    /// in `code_status`) returns `Some`.
    #[test]
    fn test_get_finds_handle_after_get_or_create() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        let mut mgr = CodebaseIndexManager::new();

        // Before lazy-start: get() returns None.
        assert!(mgr.get(&root).is_none(), "no index before get_or_create");

        // Lazy-start: this is what start_codebase_index_for_code_nav calls.
        let (handle, was_new) = mgr.get_or_create(root.clone());
        assert!(was_new, "first get_or_create must report newly spawned");
        assert_eq!(mgr.active_count(), 1);

        let found = mgr
            .get(&root)
            .expect("index must be visible after get_or_create");
        assert!(
            Arc::ptr_eq(&handle, &found),
            "get() must return the same Arc as get_or_create() — dedup / reuse"
        );
    }

    /// The index is reaped once the last strong handle drops.
    ///
    /// This is the eviction guarantee: the manager holds only a `Weak`, so when
    /// the last session pinning a git-root is torn down (here simulated by
    /// dropping the sole `Arc`), `get()` stops returning the handle and the
    /// entry no longer counts as active — no per-repo accumulation in a
    /// long-lived leader process.
    #[test]
    fn test_index_evicted_when_last_strong_ref_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        let mut mgr = CodebaseIndexManager::new();
        let (handle, _) = mgr.get_or_create(root.clone());
        assert_eq!(mgr.active_count(), 1, "index live while strong ref held");
        assert!(mgr.get(&root).is_some());

        // The agent (here: this test) was the only strong owner.
        drop(handle);

        assert!(
            mgr.get(&root).is_none(),
            "index must be released once the last strong ref drops"
        );
        assert_eq!(
            mgr.active_count(),
            0,
            "a reaped index must not count as active"
        );
    }

    /// Shared index across sessions: reaped only after the last strong ref drops.
    #[test]
    fn test_shared_index_evicted_only_after_last_session_drops() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        let mut mgr = CodebaseIndexManager::new();

        let (session_a, was_new_a) = mgr.get_or_create(root.clone());
        let (session_b, was_new_b) = mgr.get_or_create(root.clone());

        assert!(was_new_a, "first session must spawn the index");
        assert!(!was_new_b, "second session must reuse the shared index");
        assert!(
            Arc::ptr_eq(&session_a, &session_b),
            "both sessions must share one index handle (Arc::ptr_eq)"
        );
        assert_eq!(
            mgr.active_count(),
            1,
            "two sessions in one git-root back exactly one index"
        );

        // First session ends: the index stays warm for the surviving session.
        drop(session_a);
        assert!(
            mgr.get(&root).is_some(),
            "index must survive while another session still pins it"
        );
        assert_eq!(
            mgr.active_count(),
            1,
            "index still live after only the first session drops"
        );

        // Last session ends: now — and only now — the index is reaped.
        drop(session_b);
        assert!(
            mgr.get(&root).is_none(),
            "index must be released once the LAST session drops its ref"
        );
        assert_eq!(
            mgr.active_count(),
            0,
            "a reaped shared index must not count as active"
        );
    }

    /// A second get_or_create() for the same path reuses the existing handle.
    ///
    /// This corresponds to "subsequent code-nav requests reuse the same shared
    /// handle" from the plan: once the index is running, all subsequent
    /// get_or_create calls for the same path return the existing Arc.
    #[test]
    fn test_get_or_create_is_idempotent_for_same_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        let mut mgr = CodebaseIndexManager::new();
        let (h1, was_new_1) = mgr.get_or_create(root.clone());
        let (h2, was_new_2) = mgr.get_or_create(root.clone());

        assert!(was_new_1, "first call must report newly spawned");
        assert!(
            !was_new_2,
            "second call must report reused (not newly spawned)"
        );
        assert!(
            Arc::ptr_eq(&h1, &h2),
            "second get_or_create must return the same Arc as the first (index reuse)"
        );
        assert_eq!(mgr.active_count(), 1, "only one index for one path");
    }
}
