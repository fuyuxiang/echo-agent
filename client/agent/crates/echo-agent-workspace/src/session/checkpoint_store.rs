//! Disk-backed, co-located checkpoint store.
//!
//! Each finalized [`RewindCheckpoint`] is mirrored to a small on-disk store that
//! lives *inside* the session working tree (the snapshotted rootfs), so the
//! per-turn rootfs snapshot carries serialized checkpoints across a sandbox
//! restore. A restored session rehydrates them into the in-memory cache (see
//! [`CheckpointStore::with_cap`]); the cache is the hot read path, disk the
//! durable copy. Re-seeding the *live* trackers from the cache is not yet wired.
//!
//! The store is a durability **mirror**, not the restore mechanism: in-session
//! [`rewind_to`](crate::handle::WorkspaceHandle::rewind_to) always reverts
//! in-process, never via a rootfs rollback. All disk I/O is gated by
//! `workspace_rewind_durable` ([`rewind_durable_enabled`](super::checkpoint::rewind_durable_enabled));
//! off ⇒ the legacy in-memory-only path.
//!
//! **On-disk layout** (under the session `cwd`):
//!
//! ```text
//! <cwd>/.echo-agent/rewind-checkpoints/
//!   .gitignore                          # "*" — blobs are never committed
//!   <session_id>/
//!     checkpoint-<prompt_index>.json    # one RewindCheckpoint per prompt
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

use crate::session::checkpoint::RewindCheckpoint;

/// Directory (under `<cwd>/.echo-agent`) holding every session's checkpoint store.
const STORE_SUBDIR: &str = "rewind-checkpoints";

/// Default cap on retained checkpoints per session. Bounds on-disk and in-memory
/// size; the oldest (lowest `prompt_index`) are evicted beyond this.
const DEFAULT_CHECKPOINT_CAP: usize = 64;

/// Monotonic counter making each checkpoint temp-file name unique within the
/// process, so concurrent writers for the same `prompt_index` never collide.
static TMP_WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Disk-backed, co-located checkpoint store fronted by an in-memory cache.
///
/// See the [module docs](self) for the on-disk layout and durability rationale.
pub(crate) struct CheckpointStore {
    /// Per-session store directory: `<cwd>/.echo-agent/rewind-checkpoints/<session_id>`.
    dir: PathBuf,
    /// Rewritten store dir after a virtualization remount. First bind may
    /// construct the store under `/workspace`; rebind with `session_root`
    /// must persist under the real session tree.
    remounted_dir: std::sync::OnceLock<PathBuf>,
    /// Max retained checkpoints; the oldest are evicted beyond this.
    cap: usize,
    /// In-memory cache fronting disk (the hot read path). A `BTreeMap` keeps keys
    /// ordered, so the oldest prompt (smallest key) is cheap to find for eviction.
    cache: Mutex<BTreeMap<usize, RewindCheckpoint>>,
    /// Serializes `persist` against `truncate_from` so a finalize and a rewind
    /// can't interleave their disk + cache mutations and drift out of sync.
    io_lock: Mutex<()>,
}

impl CheckpointStore {
    /// Build a store for `session_id` rooted at the session `cwd`.
    ///
    /// With the durable flag **off** this does no disk I/O. With it **on** it
    /// rehydrates the cache from any blobs the rootfs snapshot carried (see
    /// [`with_cap`](Self::with_cap)).
    pub(crate) fn new(cwd: &Path, session_id: &str) -> Self {
        Self::with_cap(cwd, session_id, DEFAULT_CHECKPOINT_CAP)
    }

    /// Like [`new`](Self::new) but with an explicit retention cap (clamped to ≥1).
    /// With the durable flag on, rehydrates the cache from the blobs the rootfs
    /// snapshot carried and enforces the cap against them; flag off ⇒ empty, no I/O.
    pub(crate) fn with_cap(cwd: &Path, session_id: &str, cap: usize) -> Self {
        // `session_id` is RPC-controlled: never join it verbatim (a `../../etc`
        // would escape the store root). Map it to a safe, collision-free name first.
        let dir = cwd
            .join(".echo-agent")
            .join(STORE_SUBDIR)
            .join(session_store_dir_name(session_id));
        let cap = cap.max(1);
        let cache = if super::checkpoint::rewind_durable_enabled() {
            rehydrate_off_runtime(&dir, cap)
        } else {
            BTreeMap::new()
        };
        Self {
            dir,
            remounted_dir: std::sync::OnceLock::new(),
            cap,
            cache: Mutex::new(cache),
            io_lock: Mutex::new(()),
        }
    }

    fn store_dir(&self) -> &Path {
        self.remounted_dir.get().unwrap_or(&self.dir)
    }

    /// Point subsequent persists at `cwd` after path-virtualization remount.
    /// Copies already-written blobs so rewind does not orphan pre-virt state.
    /// Blocking `std::fs` runs in `spawn_blocking`; a failed copy leaves
    /// `remounted_dir` unset so persist keeps writing the source tree.
    pub(crate) async fn remount(&self, cwd: &Path, session_id: &str) -> bool {
        let dest = cwd
            .join(".echo-agent")
            .join(STORE_SUBDIR)
            .join(session_store_dir_name(session_id));
        let _io = self.io_lock.lock().await;
        if let Some(existing) = self.remounted_dir.get() {
            return existing == dest.as_path();
        }
        let src = self.store_dir().to_path_buf();
        if src != dest.as_path() && src.exists() {
            let dest_copy = dest.clone();
            let copy_ok =
                tokio::task::spawn_blocking(move || copy_checkpoint_blobs(&src, &dest_copy))
                    .await
                    .unwrap_or(false);
            if !copy_ok {
                tracing::warn!(
                    src = %self.store_dir().display(),
                    dest = %dest.display(),
                    "rewind checkpoint remount copy failed; keeping source dir"
                );
                return false;
            }
        }
        self.remounted_dir.set(dest).is_ok()
    }

    /// On-disk path for a single checkpoint.
    fn checkpoint_path(&self, prompt_index: usize) -> PathBuf {
        checkpoint_file_path(self.store_dir(), prompt_index)
    }

    /// Write `checkpoint` through to disk and the cache (last-write-wins per
    /// `prompt_index`), then evict the oldest beyond the cap. Takes the checkpoint
    /// by value to avoid cloning its (potentially large) contents on the hot path.
    pub(crate) async fn persist(&self, checkpoint: RewindCheckpoint) {
        // Serialize against `truncate_from` so a finalize and a rewind can't
        // interleave and leave the cache and disk inconsistent.
        let _io = self.io_lock.lock().await;

        let prompt_index = checkpoint.prompt_index;
        // Skip a checkpoint below the retention window: it would be evicted the
        // instant it's inserted (write-then-delete). Overwrites of an existing
        // index, or any index while under the cap, are always retained.
        {
            let cache = self.cache.lock().await;
            let below_window = cache.len() >= self.cap
                && !cache.contains_key(&prompt_index)
                && cache
                    .keys()
                    .next()
                    .is_some_and(|&oldest| prompt_index < oldest);
            if below_window {
                return;
            }
        }

        if let Err(e) = self.ensure_store_dir().await {
            tracing::warn!(
                error = %e,
                dir = %self.store_dir().display(),
                "rewind checkpoint store: mkdir failed; skipping persist"
            );
            return;
        }
        if let Err(e) = self.write_checkpoint_file(&checkpoint).await {
            tracing::warn!(
                error = %e,
                prompt_index,
                "rewind checkpoint store: write failed; skipping persist"
            );
            return;
        }

        // Pick cap victims under the cache lock, then release it before the file
        // deletions (never hold the mutex across I/O). The just-inserted index is
        // guaranteed to survive eviction by the retention-window check above.
        let evicted = {
            let mut cache = self.cache.lock().await;
            cache.insert(prompt_index, checkpoint);
            let mut evicted = Vec::new();
            while cache.len() > self.cap {
                // `pop_first` removes the smallest key — the oldest prompt.
                let Some((oldest, _)) = cache.pop_first() else {
                    break;
                };
                evicted.push(oldest);
            }
            evicted
        };
        for idx in evicted {
            let _ = tokio::fs::remove_file(self.checkpoint_path(idx)).await;
        }
    }

    /// Drop persisted checkpoints `>= target` from cache and disk. Scans the
    /// directory (not just the cache) so it stays correct when the cache is cold
    /// (e.g. after a sandbox restore). Missing store dir is a no-op.
    pub(crate) async fn truncate_from(&self, target: usize) {
        // Serialize against `persist` (see [`persist`](Self::persist)) so a rewind
        // and a concurrent finalize can't interleave their cache + disk mutations.
        let _io = self.io_lock.lock().await;

        // Open the disk scan *before* pruning the cache so the two can't diverge:
        // if the dir can't be opened while it still holds `>= target` blobs, a pruned
        // cache would let a later rehydrate resurrect the just-rewound checkpoints.
        let mut entries = match tokio::fs::read_dir(&self.store_dir()).await {
            Ok(entries) => entries,
            // Dir absent ⇒ no on-disk blobs to diverge from; safe to prune the cache.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.cache.lock().await.retain(|&idx, _| idx < target);
                return;
            }
            // Unscannable dir may still hold `>= target` blobs: keep the cache so it
            // stays consistent with disk (the rewound checkpoints aren't resurrected).
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    dir = %self.store_dir().display(),
                    "rewind checkpoint store: truncate scan failed; keeping cache to stay consistent with disk"
                );
                return;
            }
        };
        // Scan opened: prune the cache, then delete the `>= target` blobs below.
        self.cache.lock().await.retain(|&idx, _| idx < target);
        // Explicit loop (not `while let Ok(Some(..))`) so a transient mid-scan
        // `read_dir` error continues instead of ending early: aborting would leave
        // a `>= target` blob after the cache was pruned, and a later rehydrate would
        // resurrect that rewound checkpoint. `remove_file` failures are logged.
        loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    let name = entry.file_name();
                    if let Some(idx) = parse_checkpoint_index(&name)
                        && idx >= target
                        && let Err(e) = tokio::fs::remove_file(entry.path()).await
                    {
                        tracing::warn!(
                            error = %e,
                            path = %entry.path().display(),
                            "rewind checkpoint store: failed to remove checkpoint on truncate; \
                             it may be resurrected on a later rehydrate"
                        );
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        dir = %self.store_dir().display(),
                        "rewind checkpoint store: truncate scan read_dir error; \
                         continuing to scan remaining entries"
                    );
                    continue;
                }
            }
        }
    }

    /// Create the store dir and its `.gitignore` (idempotent). The `.gitignore`
    /// at the `rewind-checkpoints` root uses `*` so no blob is ever committed.
    async fn ensure_store_dir(&self) -> std::io::Result<()> {
        tokio::fs::create_dir_all(&self.store_dir()).await?;
        if let Some(root) = self.store_dir().parent() {
            let gitignore = root.join(".gitignore");
            // `Path::exists` is a blocking `stat` on the async runtime thread; use
            // the async probe to stay consistent with the surrounding tokio::fs I/O.
            if !tokio::fs::try_exists(&gitignore).await.unwrap_or(false) {
                tokio::fs::write(&gitignore, "*\n").await?;
            }
        }
        Ok(())
    }

    /// Serialize `checkpoint` via temp-file + rename, so a crash mid-write can't
    /// leave a torn JSON blob at the final path. The temp path carries a per-write
    /// unique suffix (pid + counter), not just `prompt_index`: two overlapping
    /// persists of the same prompt would otherwise share one temp file and tear.
    async fn write_checkpoint_file(&self, checkpoint: &RewindCheckpoint) -> std::io::Result<()> {
        let json = serde_json::to_vec(checkpoint).map_err(std::io::Error::other)?;
        let final_path = self.checkpoint_path(checkpoint.prompt_index);
        let unique = TMP_WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp_path = self.store_dir().join(format!(
            "checkpoint-{}.json.tmp.{}.{}",
            checkpoint.prompt_index,
            std::process::id(),
            unique,
        ));
        // Flush the blob to disk *before* the rename: atomic rename gives visibility,
        // not data persistence, so without this fsync the durability mechanism (a
        // rootfs snapshot carrying these files) could capture a zero-length/short
        // blob. `sync_all` fsyncs contents + metadata.
        {
            let mut f = tokio::fs::File::create(&tmp_path).await?;
            f.write_all(&json).await?;
            f.sync_all().await?;
        }
        tokio::fs::rename(&tmp_path, &final_path).await?;
        // Best-effort dir fsync so the rename (the new dir entry) is itself durable;
        // async open keeps this off the blocking path. Ignored where unsupported.
        if let Ok(dir) = tokio::fs::File::open(&self.store_dir()).await {
            let _ = dir.sync_all().await;
        }
        Ok(())
    }
}

/// On-disk path for a single checkpoint under `dir`.
fn checkpoint_file_path(dir: &Path, prompt_index: usize) -> PathBuf {
    dir.join(format!("checkpoint-{prompt_index}.json"))
}

/// Derive the on-disk store directory name for a caller-controlled `session_id`.
/// Must be (1) a single traversal-safe component (`../../etc` must not escape the
/// root) and (2) collision-free across distinct raw ids. A readable sanitized
/// prefix (`[^A-Za-z0-9_-]` → `_`, length-bounded) plus a short hash of the *raw*
/// id; deterministic so a restored session reads back the same directory.
fn session_store_dir_name(session_id: &str) -> String {
    const PREFIX_MAX: usize = 48;
    let prefix: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(PREFIX_MAX)
        .collect();
    // The hash makes the name collision-resistant; appended unconditionally so
    // the result always contains `-<hex>` and can never be empty, `.`, or `..`.
    format!("{prefix}-{:016x}", fnv1a_64(session_id.as_bytes()))
}

/// FNV-1a 64-bit hash. Small and fully specified, so the digest is stable across
/// platforms and toolchains — required since the store dir name depends on it.
fn fnv1a_64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Blocking blob copy for remount. Returns false if dest cannot be created
/// or any missing blob fails to copy.
fn copy_checkpoint_blobs(src: &Path, dest: &Path) -> bool {
    if std::fs::create_dir_all(dest).is_err() {
        return false;
    }
    let entries = match std::fs::read_dir(src) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries.flatten() {
        let from = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            return false;
        };
        if meta.file_type().is_symlink() || meta.is_dir() {
            continue;
        }
        let to = dest.join(entry.file_name());
        // Overwrite dest so a retry after a partial copy cannot keep stale
        // blobs while persist has advanced the source tree.
        if std::fs::copy(&from, &to).is_err() {
            return false;
        }
    }
    true
}

/// Run the blocking disk rehydrate without starving the async runtime.
/// [`load_capped_from_disk`] uses blocking `std::fs` but the constructor is sync
/// and reached from async paths: on a multi-thread runtime hand it to
/// `block_in_place`; otherwise (current-thread, where that panics) run inline.
fn rehydrate_off_runtime(dir: &Path, cap: usize) -> BTreeMap<usize, RewindCheckpoint> {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if handle.runtime_flavor() == RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(|| load_capped_from_disk(dir, cap))
        }
        _ => load_capped_from_disk(dir, cap),
    }
}

/// Load persisted checkpoints from `dir`, trimmed to the newest `cap` (older
/// blobs are deleted, bounding on-disk size). Blocking `std::fs`, run once at
/// construction. Missing dir ⇒ empty; unreadable/corrupt blobs are skipped.
fn load_capped_from_disk(dir: &Path, cap: usize) -> BTreeMap<usize, RewindCheckpoint> {
    let mut loaded = BTreeMap::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return loaded,
        Err(e) => {
            tracing::warn!(
                error = %e,
                dir = %dir.display(),
                "rewind checkpoint store: rehydrate scan failed"
            );
            return loaded;
        }
    };
    for entry in entries {
        // Don't `flatten()` away per-entry errors: a dropped entry would omit a blob
        // from the cache while leaving it on disk, diverging the two. Log and skip.
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    dir = %dir.display(),
                    "rewind checkpoint store: skipping unreadable dir entry on rehydrate"
                );
                continue;
            }
        };
        let file_name = entry.file_name();
        let Some(idx) = parse_checkpoint_index(&file_name) else {
            // Sweep orphaned temp files from a crashed `write_checkpoint_file`:
            // rehydrate runs once at construction before this instance writes, so
            // removing them is safe and bounds clutter. Best-effort.
            if is_orphan_checkpoint_tmp(&file_name) {
                let _ = std::fs::remove_file(entry.path());
            }
            continue;
        };
        match std::fs::read(entry.path()) {
            Ok(bytes) => match serde_json::from_slice::<RewindCheckpoint>(&bytes) {
                Ok(checkpoint) => {
                    loaded.insert(idx, checkpoint);
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    path = %entry.path().display(),
                    "rewind checkpoint store: skipping unparseable blob on rehydrate"
                ),
            },
            Err(e) => tracing::warn!(
                error = %e,
                path = %entry.path().display(),
                "rewind checkpoint store: skipping unreadable blob on rehydrate"
            ),
        }
    }
    while loaded.len() > cap {
        let Some((oldest, _)) = loaded.pop_first() else {
            break;
        };
        let _ = std::fs::remove_file(checkpoint_file_path(dir, oldest));
    }
    loaded
}

/// Parse `checkpoint-<n>.json` → `n`. `None` for anything else (including the
/// in-flight `checkpoint-<n>.json.tmp` written by `write_checkpoint_file`).
fn parse_checkpoint_index(file_name: &std::ffi::OsStr) -> Option<usize> {
    file_name
        .to_str()?
        .strip_prefix("checkpoint-")?
        .strip_suffix(".json")?
        .parse()
        .ok()
}

/// Whether `file_name` is an orphaned checkpoint temp file
/// (`checkpoint-<idx>.json.tmp[...]`) left by a crashed `write_checkpoint_file`,
/// swept on rehydrate (`parse_checkpoint_index` deliberately skips them).
fn is_orphan_checkpoint_tmp(file_name: &std::ffi::OsStr) -> bool {
    file_name
        .to_str()
        .is_some_and(|n| n.starts_with("checkpoint-") && n.contains(".json.tmp"))
}

#[cfg(test)]
impl CheckpointStore {
    /// The per-session store directory.
    pub(crate) fn dir(&self) -> &Path {
        self.store_dir()
    }

    /// Read a checkpoint, preferring the cache and falling back to a disk read
    /// (warming the cache). Test-only: the prod consumer that re-seeds live
    /// trackers from these blobs after a sandbox restore is not yet wired.
    pub(crate) async fn get(&self, prompt_index: usize) -> Option<RewindCheckpoint> {
        if let Some(cp) = self.cache.lock().await.get(&prompt_index).cloned() {
            return Some(cp);
        }
        let bytes = tokio::fs::read(self.checkpoint_path(prompt_index))
            .await
            .ok()?;
        let checkpoint: RewindCheckpoint = serde_json::from_slice(&bytes).ok()?;
        self.cache
            .lock()
            .await
            .insert(prompt_index, checkpoint.clone());
        Some(checkpoint)
    }

    /// Number of checkpoints currently held in the in-memory cache.
    async fn cached_len(&self) -> usize {
        self.cache.lock().await.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::file_state::{FileSnapshot, RewindPoint};
    use echo_agent_paths::RelPathBuf;

    /// A minimal FS-only checkpoint (no hunk delta) for store-mechanics tests.
    /// Distinct content per prompt so a disk round-trip is meaningfully checked.
    fn fs_only_checkpoint(prompt_index: usize) -> RewindCheckpoint {
        let mut fs = RewindPoint::new(prompt_index);
        fs.add_snapshot(FileSnapshot::new(
            RelPathBuf::new("a.rs").unwrap(),
            Some(format!("content for prompt {prompt_index}")),
        ));
        RewindCheckpoint {
            prompt_index,
            fs,
            hunks: None,
        }
    }

    #[tokio::test]
    async fn persist_writes_under_cwd_and_gitignores_blobs() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path(), "sess-1");

        // The store is co-located inside the working tree (the snapshotted rootfs).
        assert!(
            store.dir().starts_with(tmp.path()),
            "store dir must live inside the session working tree, got {}",
            store.dir().display()
        );

        store.persist(fs_only_checkpoint(0)).await;

        // The checkpoint blob is on disk...
        assert!(store.checkpoint_path(0).exists(), "checkpoint blob written");
        // ...and a `.gitignore` ignores the whole store so blobs are never committed.
        let gitignore = tmp
            .path()
            .join(".echo-agent")
            .join(STORE_SUBDIR)
            .join(".gitignore");
        let body = std::fs::read_to_string(&gitignore).expect("gitignore written");
        assert_eq!(body.trim(), "*", "store .gitignore must ignore all blobs");
    }

    #[tokio::test]
    async fn cap_eviction_drops_oldest_checkpoint_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::with_cap(tmp.path(), "sess-1", 2);

        for idx in 0..3 {
            store.persist(fs_only_checkpoint(idx)).await;
        }

        // The oldest (0) is evicted from both cache and disk; the last `cap` stay.
        assert!(
            !store.checkpoint_path(0).exists(),
            "evicted checkpoint's file must be removed"
        );
        assert!(store.checkpoint_path(1).exists());
        assert!(store.checkpoint_path(2).exists());
        assert!(store.get(0).await.is_none(), "evicted checkpoint is gone");
        assert!(store.get(2).await.is_some());
        assert_eq!(store.cached_len().await, 2, "cache is bounded to the cap");
    }

    #[tokio::test]
    async fn persist_below_retention_window_is_skipped_not_self_deleted() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::with_cap(tmp.path(), "sess-1", 2);
        store.persist(fs_only_checkpoint(5)).await;
        store.persist(fs_only_checkpoint(10)).await;

        // An index below the retained window would be evicted the moment it's
        // inserted, so it must be skipped entirely — not written-then-deleted.
        store.persist(fs_only_checkpoint(3)).await;
        assert!(
            !store.checkpoint_path(3).exists(),
            "below-window write must be skipped, not left dangling or self-deleted"
        );
        assert!(
            store.checkpoint_path(5).exists(),
            "existing in-window checkpoint must survive"
        );
        assert!(store.checkpoint_path(10).exists());
        assert!(store.get(3).await.is_none());
        assert!(store.get(5).await.is_some());
        assert_eq!(store.cached_len().await, 2);

        // A newer index correctly evicts the oldest and survives itself.
        store.persist(fs_only_checkpoint(11)).await;
        assert!(
            store.checkpoint_path(11).exists(),
            "the newest write must survive eviction"
        );
        assert!(
            !store.checkpoint_path(5).exists(),
            "the oldest in-window entry is evicted by the newer write"
        );
        assert!(store.checkpoint_path(10).exists());
    }

    #[tokio::test]
    async fn get_rehydrates_from_disk_when_cache_is_cold() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let store = CheckpointStore::new(tmp.path(), "sess-1");
            store.persist(fs_only_checkpoint(7)).await;
        }

        // A fresh store models a sandbox restore: cold cache, blobs on disk.
        let restored = CheckpointStore::new(tmp.path(), "sess-1");
        assert_eq!(restored.cached_len().await, 0, "fresh store starts cold");

        let cp = restored
            .get(7)
            .await
            .expect("checkpoint survives store re-creation (carried by rootfs snapshot)");
        assert_eq!(cp.prompt_index, 7);
        assert_eq!(
            cp.fs.file_snapshots.len(),
            1,
            "fs snapshot content survives the disk round-trip"
        );
        // The disk read warms the cache for subsequent hot reads.
        assert_eq!(restored.cached_len().await, 1);
    }

    #[tokio::test]
    async fn truncate_from_drops_target_and_later_checkpoints() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path(), "sess-1");
        for idx in 0..4 {
            store.persist(fs_only_checkpoint(idx)).await;
        }

        store.truncate_from(2).await;

        assert!(store.checkpoint_path(0).exists());
        assert!(store.checkpoint_path(1).exists());
        assert!(
            !store.checkpoint_path(2).exists(),
            "target checkpoint file removed"
        );
        assert!(
            !store.checkpoint_path(3).exists(),
            "later checkpoint file removed"
        );
        assert!(store.get(3).await.is_none());
        assert!(store.get(1).await.is_some());
    }

    #[tokio::test]
    async fn truncate_on_missing_store_dir_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let store = CheckpointStore::new(tmp.path(), "sess-1");
        // Nothing persisted yet (dir absent) — truncate must not error or create it.
        store.truncate_from(0).await;
        assert!(!store.dir().exists());
    }

    /// Rehydrate (post-restore): blobs the rootfs snapshot carried load back into
    /// the cache, and the cap is enforced against the on-disk set (oldest deleted).
    #[tokio::test]
    async fn rehydrate_loads_capped_set_from_disk() {
        let tmp = tempfile::tempdir().unwrap();

        // Write 4 blobs to disk (uncapped) to model a snapshot carrying history.
        let writer = CheckpointStore::with_cap(tmp.path(), "sess-1", 100);
        for idx in 0..4 {
            writer.persist(fs_only_checkpoint(idx)).await;
        }

        // Rehydrate with cap 2: only the newest 2 load, older blobs are deleted.
        let loaded = load_capped_from_disk(&writer.dir, 2);
        assert_eq!(loaded.len(), 2, "rehydrate keeps only the newest `cap`");
        assert!(loaded.contains_key(&2));
        assert!(loaded.contains_key(&3));
        assert!(
            !checkpoint_file_path(&writer.dir, 0).exists(),
            "cap is enforced against on-disk blobs, not just the cache"
        );
        assert!(!checkpoint_file_path(&writer.dir, 1).exists());
        assert!(checkpoint_file_path(&writer.dir, 2).exists());
        assert!(checkpoint_file_path(&writer.dir, 3).exists());
    }

    #[tokio::test]
    async fn rehydrate_sweeps_orphan_tmp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let writer = CheckpointStore::with_cap(tmp.path(), "sess-1", 100);
        writer.persist(fs_only_checkpoint(0)).await;

        // Simulate a crash / failed rename leaving an orphan temp file behind.
        let orphan = writer.dir.join("checkpoint-9.json.tmp.4242.0");
        std::fs::write(&orphan, b"partial").unwrap();
        assert!(orphan.exists());

        // Rehydrate sweeps the orphan and still loads the real checkpoint.
        let loaded = load_capped_from_disk(&writer.dir, 100);
        assert!(loaded.contains_key(&0), "real checkpoint still rehydrates");
        assert!(
            !orphan.exists(),
            "orphaned *.json.tmp* file must be swept on rehydrate"
        );
        // The committed checkpoint blob is untouched by the sweep.
        assert!(checkpoint_file_path(&writer.dir, 0).exists());
    }

    #[test]
    fn is_orphan_checkpoint_tmp_matches_only_temp_files() {
        use std::ffi::OsStr;
        assert!(is_orphan_checkpoint_tmp(OsStr::new(
            "checkpoint-3.json.tmp.123.0"
        )));
        assert!(is_orphan_checkpoint_tmp(OsStr::new(
            "checkpoint-3.json.tmp"
        )));
        assert!(!is_orphan_checkpoint_tmp(OsStr::new("checkpoint-3.json")));
        assert!(!is_orphan_checkpoint_tmp(OsStr::new(".gitignore")));
        assert!(!is_orphan_checkpoint_tmp(OsStr::new("other.json.tmp.1.2")));
    }

    #[test]
    fn session_store_dir_name_is_safe_and_collision_free() {
        let root = Path::new("/store/root");
        for raw in [
            "../../etc/passwd",
            "a/b",
            "..",
            ".",
            "",
            "a\\b",
            "/abs",
            "./../x",
        ] {
            let s = session_store_dir_name(raw);
            assert!(!s.is_empty(), "never empty for {raw:?}");
            assert!(
                !s.contains('/') && !s.contains('\\'),
                "no separators for {raw:?}: {s:?}"
            );
            assert!(s != "." && s != "..", "not a traversal component: {s:?}");
            // Joining must stay within the store root and add exactly one path
            // component (no `..` escape).
            let joined = root.join(&s);
            assert!(joined.starts_with(root), "stays in root: {joined:?}");
            assert_eq!(
                joined.components().count(),
                root.components().count() + 1,
                "exactly one extra component for {raw:?}: {joined:?}"
            );
        }

        // Distinct raw ids sharing a sanitized prefix must still map to distinct
        // directories (hash suffix differs), so sessions can't clobber each other.
        assert_ne!(
            session_store_dir_name("foo/bar"),
            session_store_dir_name("foo_bar"),
            "distinct raw ids must not share a store directory"
        );
        assert_ne!(session_store_dir_name("a/b"), session_store_dir_name("a-b"));

        // Deterministic: the same raw id always maps to the same directory, so a
        // restored session rehydrates from the right place.
        assert_eq!(
            session_store_dir_name("session-123"),
            session_store_dir_name("session-123")
        );

        // The readable prefix is preserved for typical ids.
        assert!(session_store_dir_name("main").starts_with("main-"));
        assert!(session_store_dir_name("a1b2-c3d4_e5").starts_with("a1b2-c3d4_e5-"));
    }
}
