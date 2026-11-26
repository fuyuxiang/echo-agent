//! Process-wide sharing of [`FsEventSource`]s — one live watcher per
//! canonical directory, reference-counted by subscribers — plus the
//! create/reuse stats that measure it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};

use tokio::runtime::Handle;

use crate::error::FsNotifyError;
use crate::source::{FsConfig, FsEventSource};

/// Long-lived runtime that shared [`FsEventSource`] event loops run on.
///
/// Sessions are short-lived and each builds its own current-thread runtime;
/// if a shared watcher's event loop ran on the *creating* session's runtime
/// it would die when that session ended, silently breaking every other
/// subscriber for the same directory. [`set_runtime_handle`] registers a
/// process-lifetime runtime so the event loop outlives any single session.
static RUNTIME_HANDLE: OnceLock<Handle> = OnceLock::new();

/// Process-wide registry of shared sources keyed by canonical watch path.
/// Holds [`Weak`] refs so a watcher is torn down once its last subscriber
/// (the last [`Arc`] returned by [`shared`]) is dropped.
static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Weak<FsEventSource>>>> = OnceLock::new();

/// Monotonic count of OS watchers actually created by [`shared`] (cache miss).
static WATCHERS_CREATED: AtomicU64 = AtomicU64::new(0);
/// Monotonic count of [`shared`] calls that reused a live watcher (cache hit).
/// Equivalently: the number of redundant OS watchers avoided by sharing.
static WATCHERS_REUSED: AtomicU64 = AtomicU64::new(0);

/// Tracing target for shared-watcher lifecycle events. Enable with
/// `RUST_LOG=fs_watcher=debug` to watch create/reuse decisions live.
pub const STATS_TARGET: &str = "fs_watcher";

/// Snapshot of the shared-watcher registry. Use [`stats`] to read it.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct FsWatcherStats {
    /// Distinct directories backed by a live OS watcher right now.
    pub live_watchers: usize,
    /// Process-lifetime count of OS watchers created (cache misses).
    pub created_total: u64,
    /// Process-lifetime count of reuses (cache hits) — i.e. OS watchers that
    /// did **not** have to be opened because an existing one was shared.
    pub reused_total: u64,
}

/// Snapshot shared-watcher stats. Prunes dead registry entries first so
/// `live_watchers` counts only watchers that still have a subscriber.
///
/// `created_total` vs `reused_total` is the headline measure: with sharing,
/// `reused_total` grows with session/subagent count while `live_watchers`
/// stays bounded by the number of distinct working directories.
pub fn stats() -> FsWatcherStats {
    let live = {
        let mut map = registry().lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, w| w.strong_count() > 0);
        map.len()
    };
    FsWatcherStats {
        live_watchers: live,
        created_total: WATCHERS_CREATED.load(Ordering::Relaxed),
        reused_total: WATCHERS_REUSED.load(Ordering::Relaxed),
    }
}

/// Register the long-lived runtime for shared watcher event loops. Call once
/// at process startup from the main (process-lifetime) runtime. Idempotent —
/// the first registration wins; later calls are ignored.
pub fn set_runtime_handle(handle: Handle) {
    let _ = RUNTIME_HANDLE.set(handle);
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Weak<FsEventSource>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Canonicalize so symlinked / relative spellings of the same directory map
/// to one watcher. Falls back to the raw path if the dir doesn't exist yet.
fn canonical_key(cwd: &Path) -> PathBuf {
    dunce::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf())
}

/// Runtime the event loop should run on: the registered process-lifetime
/// runtime when present, otherwise the current one (tests / standalone use).
fn event_loop_handle() -> Result<Handle, FsNotifyError> {
    match RUNTIME_HANDLE.get().cloned() {
        Some(h) => Ok(h),
        None => Handle::try_current().map_err(|_| FsNotifyError::NoRuntime),
    }
}

/// Get a shared [`FsEventSource`] for `cwd`, reusing a live watcher for the
/// same canonical directory or creating one if none exists. The OS watcher is
/// dropped when the last returned [`Arc`] goes away, so callers must keep the
/// `Arc` alive for as long as they want events — and must **not** call
/// [`FsEventSource::shutdown`] (that would stop the watcher for every sharer).
///
/// `config` is honored only when a watcher is actually created; a live watcher
/// for the same directory is reused as-is regardless of the requested config.
pub fn shared(cwd: PathBuf, config: FsConfig) -> Result<Arc<FsEventSource>, FsNotifyError> {
    let key = canonical_key(&cwd);

    // Fast path: an existing live watcher for this directory.
    {
        let mut map = registry().lock().unwrap_or_else(PoisonError::into_inner);
        map.retain(|_, w| w.strong_count() > 0);
        if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
            record_reuse(&key, map.len());
            return Ok(existing);
        }
    }

    // Slow path: create the watcher *without* holding the registry lock —
    // `start_on` blocks until OS-watcher init completes (up to seconds on a
    // large tree) and we must not serialize unrelated directories behind it.
    let handle = event_loop_handle()?;
    let source = Arc::new(FsEventSource::start_on(handle, cwd, config)?);

    let mut map = registry().lock().unwrap_or_else(PoisonError::into_inner);
    // Another caller may have created the watcher while we were initializing;
    // prefer theirs and let ours drop (tearing down the redundant watcher).
    if let Some(existing) = map.get(&key).and_then(Weak::upgrade) {
        record_reuse(&key, map.len());
        return Ok(existing);
    }
    map.insert(key.clone(), Arc::downgrade(&source));
    record_create(&key, map.len());
    Ok(source)
}

fn record_reuse(key: &Path, live_watchers: usize) {
    let reused_total = WATCHERS_REUSED.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::debug!(
        target: STATS_TARGET,
        event = "reused",
        path = %key.display(),
        live_watchers,
        created_total = WATCHERS_CREATED.load(Ordering::Relaxed),
        reused_total,
        "reusing shared fs watcher (OS watch avoided)"
    );
}

fn record_create(key: &Path, live_watchers: usize) {
    let created_total = WATCHERS_CREATED.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::debug!(
        target: STATS_TARGET,
        event = "created",
        path = %key.display(),
        live_watchers,
        created_total,
        reused_total = WATCHERS_REUSED.load(Ordering::Relaxed),
        "created shared fs watcher"
    );
}
