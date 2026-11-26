//! Process-wide duty-cycle gate for in-process git status/diff walks.
//!
//! libgit2 `statuses()` / diffs `pread` the same ODB packs under a process mutex;
//! concurrent walks on a shallow monorepo pin CPU and tens of GB of footprint.
//! Identical in-flight work is joined and a short snapshot is reused so client
//! spam cannot start a second pack walk. Waiter timeout does not cancel or
//! detach the walk: this `run()` returns timeout and later callers join the
//! same inflight; a late `Ok` becomes the snapshot. A walk that finishes
//! under a bumped epoch is retried once; further bumps return the last
//! completed result so `run()` cannot loop without bound.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::watch;

use super::git::{GitDiscoveryResult, discover_git_root};
use crate::git_odb::{self, OdbLimiter, OdbPermit};

pub const SNAPSHOT_TTL: Duration = Duration::from_secs(1);
pub use crate::git_odb::WALK_TIMEOUT;

pub(crate) const MAX_SLOTS: usize = 64;
const MAX_KEY_STR: usize = 256;
const MAX_DIFF_PATHS: usize = 32;
const MAX_DIFF_PATHS_BYTES: usize = 2048;
const MAX_ROOT_CACHE: usize = 1024;
const MAX_EPOCH_RETRIES: u32 = 1;
const ROOT_CACHE_TTL: Duration = if cfg!(test) {
    Duration::from_millis(80)
} else {
    Duration::from_secs(30)
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FlightKey {
    root: PathBuf,
    op: FlightOp,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum FlightOp {
    Status {
        include_untracked: bool,
        include_stats: bool,
        ignore_submodules: bool,
        include_patches: bool,
    },
    Diff {
        from: String,
        to: String,
        merge_base: bool,
        include_patch: bool,
        include_content: bool,
        paths: Vec<String>,
    },
}

impl FlightOp {
    fn is_snapshot_cacheable(&self) -> bool {
        match self {
            Self::Status {
                include_patches, ..
            } => !include_patches,
            Self::Diff {
                include_patch,
                include_content,
                ..
            } => !include_patch && !include_content,
        }
    }
}

type WalkOutcome = Result<Arc<dyn Any + Send + Sync>, Arc<str>>;

struct Snapshot {
    value: Arc<dyn Any + Send + Sync>,
    taken_at: Instant,
    epoch: u64,
}

struct Inflight {
    rx: watch::Receiver<Option<WalkOutcome>>,
    epoch: u64,
}

struct Slot {
    inflight: Option<Inflight>,
    snapshot: Option<Snapshot>,
    last_used: Instant,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            inflight: None,
            snapshot: None,
            last_used: Instant::now(),
        }
    }
}

#[derive(Default)]
struct GateState {
    slots: HashMap<FlightKey, Slot>,
    epochs: HashMap<PathBuf, u64>,
}

struct GitGateInner {
    odb: OdbLimiter,
    snapshot_ttl: Duration,
    walk_timeout: Duration,
    state: Mutex<GateState>,
}

#[derive(Clone)]
pub struct GitGate {
    inner: Arc<GitGateInner>,
}

static PROCESS_GATE: LazyLock<GitGate> = LazyLock::new(GitGate::new);
static ROOT_CACHE: LazyLock<Mutex<HashMap<PathBuf, CachedRoot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

struct CachedRoot {
    root: PathBuf,
    cached_at: Instant,
}

enum Decision {
    Snapshot(Arc<dyn Any + Send + Sync>),
    Join {
        rx: watch::Receiver<Option<WalkOutcome>>,
        epoch: u64,
    },
    Lead {
        tx: watch::Sender<Option<WalkOutcome>>,
        rx: watch::Receiver<Option<WalkOutcome>>,
        epoch: u64,
    },
}

struct InflightPublisher {
    gate: GitGate,
    key: FlightKey,
    tx: Option<watch::Sender<Option<WalkOutcome>>>,
    epoch: u64,
}

impl InflightPublisher {
    fn complete(&mut self, outcome: WalkOutcome) {
        if let Some(tx) = self.tx.take() {
            self.gate.publish(&self.key, tx, self.epoch, outcome);
        }
    }
}

impl Drop for InflightPublisher {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            self.gate.publish(
                &self.key,
                tx,
                self.epoch,
                Err(Arc::from("git walk cancelled")),
            );
        }
    }
}

pub fn shared() -> &'static GitGate {
    &PROCESS_GATE
}

pub fn invalidate(root: &Path) {
    PROCESS_GATE.invalidate(root);
}

impl Default for GitGate {
    fn default() -> Self {
        Self::new()
    }
}

impl GitGate {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(GitGateInner {
                odb: git_odb::shared().clone(),
                snapshot_ttl: SNAPSHOT_TTL,
                walk_timeout: WALK_TIMEOUT,
                state: Mutex::new(GateState::default()),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_config(
        snapshot_ttl: Duration,
        walk_timeout: Duration,
        odb_permits: usize,
        odb_acquire_wait: Duration,
    ) -> Self {
        Self {
            inner: Arc::new(GitGateInner {
                odb: OdbLimiter::new(odb_permits, odb_acquire_wait),
                snapshot_ttl,
                walk_timeout,
                state: Mutex::new(GateState::default()),
            }),
        }
    }

    pub async fn acquire_odb(&self) -> Result<OdbPermit> {
        self.inner.odb.acquire().await
    }

    pub fn invalidate(&self, root: &Path) {
        let cwd = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let canon = lookup_cached_root(&cwd).or_else(|| {
            let state = self.inner.state.lock();
            state.epochs.contains_key(&cwd).then_some(cwd.clone())
        });
        match canon {
            Some(canon) => {
                tracing::debug!(root = %canon.display(), "git_gate invalidate");
                forget_cached_roots(&canon);
                let mut state = self.inner.state.lock();
                let epoch = state.epochs.entry(canon.clone()).or_insert(0);
                *epoch = epoch.saturating_add(1);
                for (key, slot) in state.slots.iter_mut() {
                    if key.root == canon {
                        slot.snapshot = None;
                    }
                }
                evict_slots(&mut state, self.inner.snapshot_ttl, None);
            }
            None => {
                tracing::debug!(
                    root = %root.display(),
                    "git_gate invalidate-all (root unresolved)"
                );
                ROOT_CACHE.lock().clear();
                let mut state = self.inner.state.lock();
                for epoch in state.epochs.values_mut() {
                    *epoch = epoch.saturating_add(1);
                }
                for slot in state.slots.values_mut() {
                    slot.snapshot = None;
                }
                evict_slots(&mut state, self.inner.snapshot_ttl, None);
            }
        }
    }

    #[tracing::instrument(skip_all, fields(root = %cwd.display()))]
    pub(crate) async fn run<T, F, Fut>(&self, cwd: &Path, op: FlightOp, walk: F) -> Result<T>
    where
        T: Clone + Send + Sync + 'static,
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        let root = canonical_git_root(cwd).await?;
        let key = FlightKey {
            root,
            op: bound_flight_op(op),
        };
        let walk = Arc::new(walk);
        let mut epoch_retries = 0;

        loop {
            let (rx, epoch) = match self.decide(&key) {
                Decision::Snapshot(value) => {
                    tracing::debug!(root = %key.root.display(), "git_gate snapshot hit");
                    return take_typed(&value);
                }
                Decision::Join { rx, epoch } => {
                    tracing::debug!(root = %key.root.display(), "git_gate join inflight");
                    (rx, epoch)
                }
                Decision::Lead { tx, rx, epoch } => {
                    self.spawn_walk(key.clone(), Arc::clone(&walk), tx, epoch);
                    (rx, epoch)
                }
            };
            match self.finish_wait(&key, rx, epoch).await {
                WaitEnd::Done(result) => return result,
                WaitEnd::Retry(result) => {
                    if epoch_retries >= MAX_EPOCH_RETRIES {
                        return result;
                    }
                    epoch_retries += 1;
                }
            }
        }
    }

    fn decide(&self, key: &FlightKey) -> Decision {
        let mut state = self.inner.state.lock();
        evict_slots(&mut state, self.inner.snapshot_ttl, Some(key));
        let epoch = state.epochs.get(&key.root).copied().unwrap_or(0);
        let ttl = self.inner.snapshot_ttl;
        let slot = state.slots.entry(key.clone()).or_default();
        slot.last_used = Instant::now();

        if let Some(snapshot) = &slot.snapshot
            && snapshot.epoch == epoch
            && snapshot.taken_at.elapsed() < ttl
        {
            return Decision::Snapshot(Arc::clone(&snapshot.value));
        }

        if let Some(inflight) = &slot.inflight {
            let dead = inflight.rx.has_changed().is_err() && inflight.rx.borrow().is_none();
            let stale = inflight.epoch != epoch;
            if dead || stale {
                slot.inflight = None;
            }
        }

        if let Some(inflight) = &slot.inflight {
            return Decision::Join {
                rx: inflight.rx.clone(),
                epoch: inflight.epoch,
            };
        }

        let (tx, rx) = watch::channel(None);
        slot.inflight = Some(Inflight {
            rx: rx.clone(),
            epoch,
        });
        Decision::Lead { tx, rx, epoch }
    }

    fn current_epoch(&self, root: &Path) -> u64 {
        self.inner
            .state
            .lock()
            .epochs
            .get(root)
            .copied()
            .unwrap_or(0)
    }

    fn live_snapshot(&self, key: &FlightKey) -> Option<Arc<dyn Any + Send + Sync>> {
        let state = self.inner.state.lock();
        let epoch = state.epochs.get(&key.root).copied().unwrap_or(0);
        let ttl = self.inner.snapshot_ttl;
        let snapshot = state.slots.get(key)?.snapshot.as_ref()?;
        if snapshot.epoch == epoch && snapshot.taken_at.elapsed() < ttl {
            Some(Arc::clone(&snapshot.value))
        } else {
            None
        }
    }

    async fn finish_wait<T: Clone + 'static>(
        &self,
        key: &FlightKey,
        rx: watch::Receiver<Option<WalkOutcome>>,
        epoch: u64,
    ) -> WaitEnd<T> {
        match tokio::time::timeout(self.inner.walk_timeout, join_inflight::<T>(rx)).await {
            Ok(result) => {
                if epoch == self.current_epoch(&key.root) {
                    WaitEnd::Done(result)
                } else {
                    WaitEnd::Retry(result)
                }
            }
            Err(_) => {
                let timed_out = Err(anyhow!(
                    "git walk timed out after {}s",
                    self.inner.walk_timeout.as_secs()
                ));
                if epoch != self.current_epoch(&key.root) {
                    WaitEnd::Retry(timed_out)
                } else if let Some(value) = self.live_snapshot(key) {
                    WaitEnd::Done(take_typed(&value))
                } else {
                    WaitEnd::Done(timed_out)
                }
            }
        }
    }

    fn publish(
        &self,
        key: &FlightKey,
        tx: watch::Sender<Option<WalkOutcome>>,
        epoch: u64,
        outcome: WalkOutcome,
    ) {
        let mut state = self.inner.state.lock();
        let current_epoch = state.epochs.get(&key.root).copied().unwrap_or(0);
        let slot = state.slots.entry(key.clone()).or_default();
        let stale = epoch != current_epoch;

        if let Ok(value) = &outcome
            && !stale
            && key.op.is_snapshot_cacheable()
        {
            slot.snapshot = Some(Snapshot {
                value: Arc::clone(value),
                taken_at: Instant::now(),
                epoch,
            });
        }

        if slot
            .inflight
            .as_ref()
            .is_some_and(|flight| flight.epoch == epoch)
        {
            slot.inflight = None;
        }
        let _ = tx.send(Some(outcome));
    }

    fn spawn_walk<T, F, Fut>(
        &self,
        key: FlightKey,
        walk: Arc<F>,
        tx: watch::Sender<Option<WalkOutcome>>,
        epoch: u64,
    ) where
        T: Clone + Send + Sync + 'static,
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T>> + Send + 'static,
    {
        // libgit2 statuses/diffs are not cooperative; aborting this task would
        // drop the ODB permit while spawn_blocking still preads packs. Waiters
        // time out locally; the walk stays attached to the slot until it ends.
        let this = self.clone();
        tokio::spawn(async move {
            let mut publisher = InflightPublisher {
                gate: this.clone(),
                key,
                tx: Some(tx),
                epoch,
            };
            let outcome = this.execute_walk(walk.as_ref()).await;
            publisher.complete(outcome);
        });
    }

    async fn execute_walk<T, F, Fut>(&self, walk: &F) -> WalkOutcome
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T>> + Send + 'static,
        T: Send + Sync + 'static,
    {
        let permit = match self.acquire_odb().await {
            Ok(permit) => permit,
            Err(error) => return Err(Arc::from(error.to_string())),
        };
        let result = walk().await;
        drop(permit);
        match result {
            Ok(value) => Ok(Arc::new(value) as Arc<dyn Any + Send + Sync>),
            Err(error) => Err(Arc::from(error.to_string())),
        }
    }

    #[cfg(test)]
    pub(crate) fn slot_count(&self) -> usize {
        self.inner.state.lock().slots.len()
    }
}

enum WaitEnd<T> {
    Done(Result<T>),
    Retry(Result<T>),
}

async fn join_inflight<T: Clone + 'static>(
    mut rx: watch::Receiver<Option<WalkOutcome>>,
) -> Result<T> {
    let outcome = loop {
        if let Some(outcome) = rx.borrow().clone() {
            break outcome;
        }
        rx.changed()
            .await
            .map_err(|_| anyhow!("git walk cancelled"))?;
    };
    outcome_to_result(&outcome)
}

fn outcome_to_result<T: Clone + 'static>(outcome: &WalkOutcome) -> Result<T> {
    match outcome {
        Ok(value) => take_typed(value),
        Err(error) => Err(anyhow!("{error}")),
    }
}

fn take_typed<T: Clone + 'static>(value: &Arc<dyn Any + Send + Sync>) -> Result<T> {
    value
        .downcast_ref::<T>()
        .cloned()
        .ok_or_else(|| anyhow!("git gate snapshot type mismatch"))
}

fn evict_slots(state: &mut GateState, ttl: Duration, keep: Option<&FlightKey>) {
    state.slots.retain(|key, slot| {
        keep.is_some_and(|keep| key == keep) || slot.inflight.is_some() || slot.snapshot.is_some()
    });
    let limit = match keep {
        Some(key) if !state.slots.contains_key(key) => MAX_SLOTS.saturating_sub(1),
        _ => MAX_SLOTS,
    };
    while state.slots.len() > limit {
        let victim = state
            .slots
            .iter()
            .filter(|(key, slot)| {
                slot.inflight.is_none() && keep.map(|keep| *key != keep).unwrap_or(true)
            })
            .min_by(|(_, left), (_, right)| {
                snapshot_expired(left, ttl)
                    .cmp(&snapshot_expired(right, ttl))
                    .reverse()
                    .then(left.last_used.cmp(&right.last_used))
            })
            .map(|(key, _)| key.clone());
        match victim {
            Some(key) => {
                state.slots.remove(&key);
            }
            None => break,
        }
    }
}

fn snapshot_expired(slot: &Slot, ttl: Duration) -> bool {
    match &slot.snapshot {
        Some(snapshot) => snapshot.taken_at.elapsed() >= ttl,
        None => true,
    }
}

fn bound_flight_op(op: FlightOp) -> FlightOp {
    match op {
        FlightOp::Status { .. } => op,
        FlightOp::Diff {
            from,
            to,
            merge_base,
            include_patch,
            include_content,
            paths,
        } => FlightOp::Diff {
            from: bound_key_str(from),
            to: bound_key_str(to),
            merge_base,
            include_patch,
            include_content,
            paths: bound_key_paths(paths),
        },
    }
}

fn bound_key_str(value: String) -> String {
    if value.len() <= MAX_KEY_STR {
        return value;
    }
    format!(
        "sha256:{}:{:x}",
        value.len(),
        Sha256::digest(value.as_bytes())
    )
}

fn bound_key_paths(paths: Vec<String>) -> Vec<String> {
    let total: usize = paths.iter().map(String::len).sum();
    let within_limits = paths.len() <= MAX_DIFF_PATHS
        && total <= MAX_DIFF_PATHS_BYTES
        && paths.iter().all(|path| path.len() <= MAX_KEY_STR);
    if within_limits {
        return paths;
    }
    let mut hasher = Sha256::new();
    hasher.update((paths.len() as u64).to_le_bytes());
    for path in &paths {
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update(path.as_bytes());
    }
    vec![format!(
        "sha256:{}:{}:{:x}",
        paths.len(),
        total,
        hasher.finalize()
    )]
}

fn lookup_cached_root(cwd: &Path) -> Option<PathBuf> {
    let cache = ROOT_CACHE.lock();
    let entry = cache.get(cwd)?;
    (entry.cached_at.elapsed() < ROOT_CACHE_TTL).then(|| entry.root.clone())
}

fn store_cached_root(cwd: PathBuf, root: PathBuf) {
    let mut cache = ROOT_CACHE.lock();
    if cache.len() >= MAX_ROOT_CACHE {
        cache.retain(|_, entry| entry.cached_at.elapsed() < ROOT_CACHE_TTL);
        if cache.len() >= MAX_ROOT_CACHE {
            cache.clear();
        }
    }
    cache.insert(
        cwd,
        CachedRoot {
            root,
            cached_at: Instant::now(),
        },
    );
}

fn forget_cached_roots(root: &Path) {
    ROOT_CACHE.lock().retain(|_, entry| entry.root != root);
}

async fn canonical_git_root(path: &Path) -> Result<PathBuf> {
    let cwd = dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(root) = lookup_cached_root(&cwd) {
        return Ok(root);
    }

    let probe = cwd.clone();
    let discovered = tokio::task::spawn_blocking(move || discover_git_root(&probe))
        .await
        .map_err(|error| anyhow!("git discover task failed: {error}"))?;
    let discovered = match discovered {
        GitDiscoveryResult::Found(root) => root,
        GitDiscoveryResult::NotARepo => {
            anyhow::bail!("not a git repository: {}", path.display())
        }
        GitDiscoveryResult::DiscoveryFailed(error) => {
            return Err(error).context(format!("git discover failed for {}", path.display()));
        }
    };
    let root = dunce::canonicalize(&discovered).unwrap_or(discovered);
    store_cached_root(cwd, root.clone());
    Ok(root)
}

#[cfg(test)]
#[path = "git_gate_tests.rs"]
mod tests;
