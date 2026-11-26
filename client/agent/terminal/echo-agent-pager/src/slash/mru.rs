//! Slash command MRU / recency (`$ECHO_AGENT_HOME/slash-mru.json`).
//!
//! Flat `command → last_used` map (canonical names). Tiebreaks use recency
//! decay (7-day half-life, 0.1 floor). Bounded to [`MAX_ENTRIES`].
//!
//! Ownership: each [`crate::slash::SlashController`] holds an
//! `Rc<RefCell<SlashMru>>` (single-threaded UI; no mutex). `AppView` owns one
//! store and injects it into every controller (agent prompts + dashboard
//! dispatch) so they stay in sync — no process-global singleton. Default and
//! test controllers get an isolated in-memory store (no disk I/O).
//!
//! Persistence: a `touch` only marks the store dirty (never blocks the UI on
//! disk). When a command is recorded, the controller hands an owned
//! [`MruSnapshot`] to [`persist_async`], which serializes writes through one
//! long-lived background thread (atomic temp-file + rename). The `Rc<RefCell>`
//! itself never crosses a thread boundary; only the `Send` snapshot does.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::util::echo_agent_home;

const RECENCY_HALF_LIFE_SECS: f64 = 7.0 * 86_400.0;
const RECENCY_FLOOR: f64 = 0.1;
const MAX_ENTRIES: usize = 256;

/// On-disk format. `by_command` is canonical; legacy `by_prefix` is migrated once.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct MruFile {
    #[serde(default)]
    by_command: HashMap<String, u64>,
    /// Legacy per-prefix schema (read-only migrate).
    #[serde(default)]
    by_prefix: HashMap<String, HashMap<String, u64>>,
}

#[derive(Debug)]
pub struct SlashMru {
    by_command: HashMap<String, u64>,
    loaded: bool,
    dirty: bool,
    /// When false (tests), never touch disk.
    persist_enabled: bool,
}

impl Default for SlashMru {
    fn default() -> Self {
        Self {
            by_command: HashMap::new(),
            loaded: false,
            dirty: false,
            persist_enabled: true,
        }
    }
}

impl SlashMru {
    pub fn new() -> Self {
        Self::default()
    }

    /// Isolated store for unit tests (no disk I/O).
    pub fn new_in_memory() -> Self {
        Self {
            loaded: true,
            persist_enabled: false,
            ..Self::default()
        }
    }

    fn store_path() -> PathBuf {
        echo_agent_home().join("slash-mru.json")
    }

    fn normalize_command(command_name: &str) -> Option<String> {
        let name = command_name.trim().trim_start_matches('/');
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Recency-with-decay tiebreak score. Pure recency (one `last_used`
    /// timestamp per command, no use-count) scaled by an exponential decay so a
    /// long-stale entry can't win ties forever; the floor keeps any prior use
    /// just above never-used.
    fn recency_score(last_used: u64, now: u64) -> u64 {
        if last_used == 0 {
            return 0;
        }
        let age = now.saturating_sub(last_used) as f64;
        let factor = (0.5_f64.powf(age / RECENCY_HALF_LIFE_SECS)).max(RECENCY_FLOOR);
        ((last_used as f64) * factor) as u64
    }

    fn ensure_loaded(&mut self) {
        if self.loaded || !self.persist_enabled {
            if !self.loaded {
                self.loaded = true;
            }
            return;
        }
        let path = Self::store_path();
        match fs::read(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                self.loaded = true;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "slash MRU: read failed; using empty store, persistence disabled for session"
                );
                // Mark loaded so we don't re-attempt the read on every
                // `rank_score` (once per candidate per keystroke on the UI
                // thread), and disable persistence so we never clobber a file
                // we couldn't read.
                self.loaded = true;
                self.persist_enabled = false;
            }
            Ok(bytes) => match serde_json::from_slice::<MruFile>(&bytes) {
                Ok(file) => {
                    self.by_command = file.by_command;
                    if self.by_command.is_empty() && !file.by_prefix.is_empty() {
                        // Collapse legacy per-prefix buckets: max timestamp per command.
                        for bucket in file.by_prefix.values() {
                            for (cmd, ts) in bucket {
                                let e = self.by_command.entry(cmd.clone()).or_insert(0);
                                *e = (*e).max(*ts);
                            }
                        }
                        self.dirty = true;
                    }
                    self.trim_to_cap();
                    self.loaded = true;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "slash MRU: corrupt file ignored"
                    );
                    self.loaded = true;
                }
            },
        }
    }

    fn trim_to_cap(&mut self) {
        if self.by_command.len() <= MAX_ENTRIES {
            return;
        }
        let mut entries: Vec<(String, u64)> = self.by_command.drain().collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        entries.truncate(MAX_ENTRIES);
        self.by_command = entries.into_iter().collect();
    }

    /// Record use of a canonical command name (ignores typed prefix; flat model).
    pub fn touch(&mut self, _typed_prefix: &str, command_name: &str) {
        let Some(cmd) = Self::normalize_command(command_name) else {
            return;
        };
        self.ensure_loaded();
        let now = Self::now_secs();
        self.by_command.insert(cmd, now);
        self.trim_to_cap();
        if self.persist_enabled {
            self.dirty = true;
        }
    }

    pub fn last_used(&mut self, _typed_prefix: &str, command_name: &str) -> u64 {
        let Some(cmd) = Self::normalize_command(command_name) else {
            return 0;
        };
        self.ensure_loaded();
        self.by_command.get(&cmd).copied().unwrap_or(0)
    }

    pub fn rank_score(&mut self, _typed_prefix: &str, command_name: &str) -> u64 {
        let ts = self.last_used("", command_name);
        Self::recency_score(ts, Self::now_secs())
    }

    /// Take an owned, `Send` snapshot to persist when dirty; clears the dirty
    /// flag. Returns `None` when persistence is disabled (tests) or nothing
    /// changed. The snapshot is written off the UI thread by [`persist_async`].
    pub fn take_persist_snapshot(&mut self) -> Option<MruSnapshot> {
        if !self.persist_enabled || !self.dirty {
            return None;
        }
        let file = MruFile {
            by_command: self.by_command.clone(),
            by_prefix: HashMap::new(),
        };
        let bytes = serde_json::to_vec(&file).ok()?;
        self.dirty = false;
        Some(MruSnapshot {
            path: Self::store_path(),
            bytes,
        })
    }

    /// Re-flag unpersisted changes after a failed write so the next
    /// [`Self::take_persist_snapshot`] retries. No-op when persistence is off.
    pub fn mark_dirty(&mut self) {
        if self.persist_enabled {
            self.dirty = true;
        }
    }

    #[cfg(test)]
    pub fn seed_for_test(&mut self, _prefix: &str, command_name: &str, last_used: u64) {
        self.loaded = true;
        self.persist_enabled = false;
        if let Some(cmd) = Self::normalize_command(command_name) {
            self.by_command.insert(cmd, last_used);
        }
    }
}

/// An owned, `Send` snapshot of the MRU ready to write to disk. Produced on
/// the UI thread by [`SlashMru::take_persist_snapshot`]; written off-thread.
#[derive(Debug)]
pub struct MruSnapshot {
    path: PathBuf,
    bytes: Vec<u8>,
}

impl MruSnapshot {
    /// Atomic write (temp file + `fsync` + rename). Returns `true` on success.
    /// Safe on a worker thread.
    fn write(&self) -> bool {
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("json.tmp");
        let write_ok = (|| -> io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&self.bytes)?;
            f.sync_all()?;
            fs::rename(&tmp, &self.path)?;
            Ok(())
        })();
        match write_ok {
            Ok(()) => true,
            Err(e) => {
                tracing::debug!(error = %e, "slash MRU: persist failed");
                let _ = fs::remove_file(&tmp);
                false
            }
        }
    }
}

/// Persist a snapshot off the UI thread. Writes are serialized through a single
/// long-lived background thread (created on first use), so concurrent accepts
/// can never reorder or tear the on-disk file. The send is non-blocking; the
/// `Rc<RefCell<SlashMru>>` never leaves the UI thread (only the `Send` snapshot
/// does).
///
/// Returns `true` if the snapshot was handed to the writer thread or written
/// synchronously; `false` only when no write could be attempted (so the caller
/// can keep the store dirty and retry on the next record). If the writer thread
/// can't be spawned, or its channel has hung up, this falls back to a
/// best-effort synchronous write rather than silently dropping the update.
///
/// The off-thread write is itself best-effort: a transient disk failure is
/// self-healing because each snapshot is the full command map (not a delta), so
/// the next `record_command_use` re-persists everything.
///
/// The writer channel is the only process-global piece — write-only I/O
/// plumbing, not shared ranking state — so it carries none of the
/// singleton/test-seam baggage that an injected store avoids.
pub fn persist_async(snapshot: MruSnapshot) -> bool {
    static WRITER: OnceLock<Option<Sender<MruSnapshot>>> = OnceLock::new();
    let tx = WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<MruSnapshot>();
        match std::thread::Builder::new()
            .name("slash-mru-writer".to_string())
            .spawn(move || {
                while let Ok(snapshot) = rx.recv() {
                    snapshot.write();
                }
            }) {
            Ok(_) => Some(tx),
            Err(e) => {
                tracing::debug!(error = %e, "slash MRU: writer thread spawn failed; writing synchronously");
                None
            }
        }
    });
    match tx {
        Some(tx) => match tx.send(snapshot) {
            Ok(()) => true,
            // Writer thread gone: best-effort synchronous write of the snapshot
            // returned in the send error rather than dropping it.
            Err(e) => e.0.write(),
        },
        // Writer thread never started: best-effort synchronous write.
        None => snapshot.write(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_is_flat_by_command() {
        let mut mru = SlashMru::new_in_memory();
        mru.touch("p", "pager-headless");
        mru.touch("q", "quit");
        assert!(mru.last_used("anything", "pager-headless") > 0);
        assert!(mru.last_used("x", "quit") > 0);
        // Flat: prefix does not scope records.
        assert_eq!(mru.last_used("p", "quit"), mru.last_used("q", "quit"));
    }

    #[test]
    fn strips_leading_slash_on_command() {
        let mut mru = SlashMru::new_in_memory();
        mru.touch("m", "/model");
        assert!(mru.last_used("", "model") > 0);
        assert_eq!(mru.last_used("", "/model"), mru.last_used("", "model"));
    }

    #[test]
    fn recency_decays_stale_entries() {
        let now = 1_700_000_000_u64;
        let recent = SlashMru::recency_score(now - 60, now);
        let week_old = SlashMru::recency_score(now - 7 * 86_400, now);
        let month_old = SlashMru::recency_score(now - 30 * 86_400, now);
        assert!(recent > week_old);
        assert!(week_old > month_old);
        assert!(month_old > 0);
        assert_eq!(SlashMru::recency_score(0, now), 0);
    }

    #[test]
    fn in_memory_store_never_dirties_for_disk() {
        let mut mru = SlashMru::new_in_memory();
        mru.touch("p", "plan");
        assert!(!mru.dirty);
        // In-memory stores never produce a persist snapshot (no disk I/O).
        assert!(mru.take_persist_snapshot().is_none());
    }

    #[test]
    fn dirty_store_yields_one_snapshot_then_clears() {
        let mut mru = SlashMru::new(); // persist-enabled
        mru.loaded = true; // avoid disk read in test
        mru.touch("p", "plan");
        assert!(mru.dirty);
        assert!(mru.take_persist_snapshot().is_some());
        // Dirty flag cleared; no redundant second write.
        assert!(!mru.dirty);
        assert!(mru.take_persist_snapshot().is_none());
    }

    #[test]
    fn mark_dirty_requeues_after_failed_write() {
        // A snapshot was taken (dirty cleared) but the write could not be
        // handed off; mark_dirty re-queues it so the next call retries.
        let mut mru = SlashMru::new();
        mru.loaded = true;
        mru.touch("p", "plan");
        assert!(mru.take_persist_snapshot().is_some());
        assert!(mru.take_persist_snapshot().is_none()); // nothing to retry yet
        mru.mark_dirty();
        assert!(mru.take_persist_snapshot().is_some()); // retried
    }

    #[test]
    fn mark_dirty_noop_when_persistence_disabled() {
        let mut mru = SlashMru::new_in_memory();
        mru.mark_dirty();
        assert!(!mru.dirty);
        assert!(mru.take_persist_snapshot().is_none());
    }
}
