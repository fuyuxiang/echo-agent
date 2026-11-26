//! Heap-profile IoC seam + threshold monitor.
//!
//! The composition root installs jemalloc ops; without a hook (tests, Windows,
//! non-jemalloc builds) every seam API is inert. The monitor polls
//! `stats.resident` and uploads dumps via `gcs::upload_file`.

mod monitor;

pub use monitor::{
    DumpAttemptOutcome, HARD_DUMP_SIZE_CAP_BYTES, HeapProfileMonitor, HeapProfileUploadHandles,
    JemallocHeapProfileConfig, SCOPED_KILL_SWITCH_INTERVAL, build_upload_handles,
    clamp_poll_interval_secs, is_valid_session_id, normalize_thresholds, object_paths,
    resolve_jemalloc_heap_profile, sanitize_version, should_latch,
};

/// Recommended jemalloc `lg_prof_sample` (2^19 bytes ≈ 512 KiB).
///
/// Keep process `MALLOC_CONF` / `_RJEM_MALLOC_CONF` and dump meta in sync with
/// this value (unit-test BUILD env and ops docs).
pub const LG_PROF_SAMPLE: u32 = 19;

use std::path::Path;
use std::sync::OnceLock;

/// Function pointers the composition root installs for jemalloc-backed ops.
pub struct HeapProfileHooks {
    pub stats: fn() -> Option<JemallocStats>,
    pub set_prof_active: fn(bool) -> bool,
    pub dump_to_path: fn(&Path) -> Result<(), String>,
    pub prof_available: fn() -> bool,
}

/// Jemalloc heap counters (bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JemallocStats {
    pub allocated: u64,
    pub resident: u64,
}

static HOOKS: OnceLock<HeapProfileHooks> = OnceLock::new();

/// Install hooks. First caller wins.
pub fn install(hooks: HeapProfileHooks) {
    let _ = HOOKS.set(hooks);
}

/// Allocated / resident bytes, or `None` if no hook or the read fails.
pub fn stats() -> Option<JemallocStats> {
    HOOKS.get().and_then(|h| (h.stats)())
}

/// Toggle heap sampling; `false` if no hook or the mallctl fails.
pub fn set_prof_active(active: bool) -> bool {
    HOOKS
        .get()
        .map(|h| (h.set_prof_active)(active))
        .unwrap_or(false)
}

/// Dump a heap profile to `path`.
pub fn dump_to_path(path: &Path) -> Result<(), String> {
    HOOKS
        .get()
        .map(|h| (h.dump_to_path)(path))
        .unwrap_or_else(|| Err("no heap profile hooks".into()))
}

/// Whether jemalloc was started with profiling (`opt.prof`).
pub fn prof_available() -> bool {
    HOOKS.get().map(|h| (h.prof_available)()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    static FAKE_ALLOCATED: AtomicU64 = AtomicU64::new(0);
    static FAKE_RESIDENT: AtomicU64 = AtomicU64::new(0);
    static FAKE_PROF_ACTIVE: AtomicBool = AtomicBool::new(false);
    static FAKE_PROF_AVAILABLE: AtomicBool = AtomicBool::new(true);
    static FAKE_DUMP_FAIL: AtomicBool = AtomicBool::new(false);
    static FAKE_STATS_NONE: AtomicBool = AtomicBool::new(false);
    static FAKE_DUMP_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

    fn fake_stats() -> Option<JemallocStats> {
        if FAKE_STATS_NONE.load(Ordering::SeqCst) {
            return None;
        }
        Some(JemallocStats {
            allocated: FAKE_ALLOCATED.load(Ordering::SeqCst),
            resident: FAKE_RESIDENT.load(Ordering::SeqCst),
        })
    }

    fn fake_set_prof_active(active: bool) -> bool {
        if !FAKE_PROF_AVAILABLE.load(Ordering::SeqCst) {
            return false;
        }
        FAKE_PROF_ACTIVE.store(active, Ordering::SeqCst);
        true
    }

    fn fake_dump_to_path(path: &Path) -> Result<(), String> {
        *FAKE_DUMP_PATH.lock().expect("dump path lock") = Some(path.to_path_buf());
        if FAKE_DUMP_FAIL.load(Ordering::SeqCst) {
            return Err("fake dump failed".into());
        }
        Ok(())
    }

    fn fake_prof_available() -> bool {
        FAKE_PROF_AVAILABLE.load(Ordering::SeqCst)
    }

    fn fake_hooks() -> HeapProfileHooks {
        HeapProfileHooks {
            stats: fake_stats,
            set_prof_active: fake_set_prof_active,
            dump_to_path: fake_dump_to_path,
            prof_available: fake_prof_available,
        }
    }

    fn reset_fakes() {
        FAKE_ALLOCATED.store(1_000, Ordering::SeqCst);
        FAKE_RESIDENT.store(2_000, Ordering::SeqCst);
        FAKE_PROF_ACTIVE.store(false, Ordering::SeqCst);
        FAKE_PROF_AVAILABLE.store(true, Ordering::SeqCst);
        FAKE_DUMP_FAIL.store(false, Ordering::SeqCst);
        FAKE_STATS_NONE.store(false, Ordering::SeqCst);
        *FAKE_DUMP_PATH.lock().expect("dump path lock") = None;
    }

    fn last_dump_path() -> Option<PathBuf> {
        FAKE_DUMP_PATH.lock().expect("dump path lock").clone()
    }

    /// `HOOKS` is process-global `OnceLock` (first-wins). This test is the
    /// **sole installer** under serial key `heap_profile_hooks` — do not call
    /// `install` from other serial groups or the empty/inert phase is order-
    /// sensitive. Inert API checks do not assert private lock emptiness so a
    /// future install site only breaks the install-path assertions, not an
    /// opaque `is_none` check.
    #[test]
    #[serial_test::serial(heap_profile_hooks)]
    fn heap_profile_inert_then_installed_hooks() {
        let already_installed = HOOKS.get().is_some();
        if !already_installed {
            assert!(stats().is_none());
            assert!(!set_prof_active(true));
            assert!(!set_prof_active(false));
            assert_eq!(
                dump_to_path(Path::new("/tmp/no-hooks.heap")).unwrap_err(),
                "no heap profile hooks"
            );
            assert!(!prof_available());
        }

        reset_fakes();
        install(fake_hooks());
        // Second install is a no-op (first-wins); only meaningful when we
        // installed fakes above.
        install(HeapProfileHooks {
            stats: || None,
            set_prof_active: |_| false,
            dump_to_path: |_| Err("second install".into()),
            prof_available: || false,
        });

        if already_installed {
            // Another installer won; cannot assert fake-hook behavior.
            return;
        }

        assert_eq!(
            stats(),
            Some(JemallocStats {
                allocated: 1_000,
                resident: 2_000,
            })
        );
        assert!(prof_available());

        assert!(set_prof_active(true));
        assert!(FAKE_PROF_ACTIVE.load(Ordering::SeqCst));
        assert!(set_prof_active(false));
        assert!(!FAKE_PROF_ACTIVE.load(Ordering::SeqCst));

        let dump_path = PathBuf::from("/tmp/fake.heap");
        dump_to_path(&dump_path).expect("dump");
        assert_eq!(last_dump_path().as_ref(), Some(&dump_path));

        FAKE_STATS_NONE.store(true, Ordering::SeqCst);
        assert!(stats().is_none());
        FAKE_STATS_NONE.store(false, Ordering::SeqCst);

        FAKE_DUMP_FAIL.store(true, Ordering::SeqCst);
        let fail_path = Path::new("/tmp/fail.heap");
        assert_eq!(dump_to_path(fail_path).unwrap_err(), "fake dump failed");
        assert_eq!(last_dump_path().as_deref(), Some(fail_path));
        FAKE_DUMP_FAIL.store(false, Ordering::SeqCst);

        FAKE_PROF_AVAILABLE.store(false, Ordering::SeqCst);
        assert!(!prof_available());
        assert!(!set_prof_active(true));
        assert!(!FAKE_PROF_ACTIVE.load(Ordering::SeqCst));
    }
}
