//! [`FsEventSource`] — owns the OS watcher, runs the async event loop, and
//! drives the lock state machine to emit semantic [`FsEvent`]s on a single
//! broadcast channel.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::error::FsNotifyError;
use crate::event::{FsEvent, GitMetaKind};
use crate::handle::{self, FsNotifyConfig as RawFsConfig, FsNotifyHandle};
use crate::merge::RawFsEvent;
use crate::paths::classify_git_path;
use crate::state::{COOLDOWN_MS, LockState, LockTransition, StaleWarn, drive};
use crate::vcs::{find_sl_dir, sapling_enabled};

const CHANNEL_CAPACITY: usize = 256;

/// Construct via `FsConfig::default()`, then assign the public fields.
/// Internal timing constants (cooldown, stale-lock) live in `crate::state`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct FsConfig {
    pub debounce_ms: u64,
    pub ignore_patterns: Vec<String>,
}

impl Default for FsConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 100,
            ignore_patterns: vec![],
        }
    }
}

/// Drop cancels the event loop and OS watcher.
///
/// **Mid-batch lock caveat:** if `.git/index.lock` appears and disappears
/// within one debounce window, the FS state is read at batch-processing
/// time — but the lock-path *events* in the batch still mark git-op
/// activity, so such fast ops produce a normal (settle-merged)
/// `GitOperationStarted`/`Completed` cycle with the head compared against
/// its last out-of-op value.
pub struct FsEventSource {
    out_tx: broadcast::Sender<FsEvent>,
    shutdown: CancellationToken,
    watcher: FsNotifyHandle,
}

impl FsEventSource {
    /// Blocks until the OS watcher initializes. Requires a tokio runtime; the
    /// event loop runs on the current runtime. Prefer [`crate::shared`] (which dedupes
    /// watchers by directory and runs the loop on the registered long-lived
    /// runtime) over creating per-caller sources.
    pub fn start(cwd: PathBuf, config: FsConfig) -> Result<Self, FsNotifyError> {
        let handle = Handle::try_current().map_err(|_| FsNotifyError::NoRuntime)?;
        Self::start_on(handle, cwd, config)
    }

    /// Like [`start`](Self::start) but runs the event loop on `handle` instead
    /// of the current runtime. Used by [`crate::shared`] so the loop lives on a
    /// process-lifetime runtime rather than a short-lived per-session one.
    pub fn start_on(handle: Handle, cwd: PathBuf, config: FsConfig) -> Result<Self, FsNotifyError> {
        let raw_config = RawFsConfig {
            debounce_ms: config.debounce_ms,
            ignore_patterns: config.ignore_patterns,
        };
        // Canonicalize once so discovery and the watcher resolve `.git`/`.sl`
        // from the *same* root. The watcher canonicalizes `cwd` internally
        // before its ancestor walk (macOS FSEvents resolves symlinks), so a
        // symlinked `cwd` passed raw to `discover_vcs` could miss `.sl` while
        // the watcher still attaches a `.sl/wlock` watch — leaking `.sl/*` as
        // workspace files and skipping revision-switch suppression. Resolving
        // here keeps `discover_vcs` (and thus `lock_present`/`is_internal`) in
        // agreement with the watcher. Falls back to the raw path if `cwd`
        // doesn't exist yet, matching the watcher's own fallback.
        let cwd = dunce::canonicalize(&cwd).unwrap_or(cwd);
        // Resolve the Sapling kill-switch once so discovery and the watcher
        // agree on whether `.sl` is active.
        let sapling = sapling_enabled();
        let (raw_rx, watcher_handle) = handle::start(cwd.clone(), raw_config, sapling)?;

        let vcs = discover_vcs(&cwd, sapling);
        let cooldown = Duration::from_millis(COOLDOWN_MS);
        let (out_tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        let shutdown = CancellationToken::new();

        // Detached: cancellation via `shutdown` (biased-selected in the loop).
        handle.spawn(event_loop(
            raw_rx,
            out_tx.clone(),
            vcs,
            cooldown,
            shutdown.clone(),
        ));

        Ok(Self {
            out_tx,
            shutdown,
            watcher: watcher_handle,
        })
    }

    /// Each subscriber has an independent backlog; lag surfaces as
    /// `Err(broadcast::error::RecvError::Lagged(n))`.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<FsEvent> {
        self.out_tx.subscribe()
    }

    /// Number of OS-level watches this source currently holds (one per
    /// directory in per-dir mode, one per `watch()` call in fan-out mode).
    /// On Linux this approximates the process's inotify watch-descriptor
    /// footprint for this source. Primarily for stats, logs, and benchmarks.
    #[must_use]
    pub fn os_watch_count(&self) -> usize {
        self.watcher.watch_count()
    }

    /// Idempotent. `Drop` also cancels.
    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }
}

impl Drop for FsEventSource {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// VCS metadata directories discovered for the watched workspace. Both VCSs are
/// considered together — a parent move in *either* flips `head_changed` (see
/// [`read_head`]), so no precedence is needed.
///
/// `sl_dir` is discovered whenever a `.sl` exists, *independent* of the
/// `sapling` kill-switch, so [`VcsDirs::is_internal`] can always anchor to it
/// and drop `.sl/*`. `sapling` gates only *suppression* (the Sapling arms of
/// [`lock_present`]/[`read_head`] and the `.sl` watch).
#[derive(Default)]
struct VcsDirs {
    /// `.git` dir from git2 discovery (handles worktrees / gitlinks).
    git_dir: Option<PathBuf>,
    /// `.sl` working-copy dir (ancestor walk), or `None` if absent.
    sl_dir: Option<PathBuf>,
    /// Whether Sapling suppression is enabled (`ECHO_AGENT_FSNOTIFY_SAPLING`).
    sapling: bool,
}

fn discover_vcs(cwd: &Path, sapling: bool) -> VcsDirs {
    VcsDirs {
        git_dir: git2::Repository::discover(cwd)
            .ok()
            .map(|r| r.path().to_path_buf()),
        sl_dir: find_sl_dir(cwd),
        sapling,
    }
}

impl VcsDirs {
    /// Git metadata only; Sapling contributes no `GitMetaKind`.
    fn classify(&self, p: &Path) -> Option<GitMetaKind> {
        self.git_dir
            .as_deref()
            .and_then(|d| classify_git_path(p, d))
    }

    /// Paths under the discovered `.git`/`.sl` dir tick the state machine but
    /// are not workspace files, so they never reach `FilesChanged`. Anchored to
    /// the discovered dirs (not a bare `.sl` component match) so an unrelated
    /// `.sl` ancestor of the watch root can't suppress real files.
    fn is_internal(&self, p: &Path) -> bool {
        self.git_dir.as_deref().is_some_and(|d| p.starts_with(d))
            || self.sl_dir.as_deref().is_some_and(|d| p.starts_with(d))
    }

    /// True only for the exact VCS lock files that arm suppression (mirrors
    /// [`lock_present`]: `index.lock`/`gc.pid` directly under the git dir,
    /// `wlock` under `.sl`): an event on one marks git-op activity even when
    /// the file is already gone by batch-processing time. Other transient
    /// `.git/*.lock` files (`config.lock`, `HEAD.lock`, per-ref locks) must
    /// NOT synthesize ops — they accompany non-op activity. Name-first
    /// comparison keeps this allocation-free on the per-path hot loop.
    fn is_lock_path(&self, p: &Path) -> bool {
        let Some(name) = p.file_name() else {
            return false;
        };
        if name == "index.lock" || name == "gc.pid" {
            return self
                .git_dir
                .as_deref()
                .is_some_and(|d| p.parent() == Some(d));
        }
        if name == "wlock" && self.sapling {
            return self
                .sl_dir
                .as_deref()
                .is_some_and(|d| p.parent() == Some(d));
        }
        false
    }
}

// Blocking FS reads on the event loop: tiny files, hot OS cache. Network
// FS users would need spawn_blocking.
fn lock_present(v: &VcsDirs) -> bool {
    let git = v
        .git_dir
        .as_deref()
        .is_some_and(|d| d.join("index.lock").exists() || d.join("gc.pid").exists());
    // Sapling's legacy `.sl/wlock` working-copy lock (store/lock guards history,
    // not the working copy); only when suppression is enabled.
    let sl = v.sapling
        && v.sl_dir
            .as_deref()
            .is_some_and(|d| d.join("wlock").exists());
    git || sl
}

/// Combined head token `"<git HEAD>|<sl p1>"`: changes iff *either* VCS moves
/// its working-copy parent, so [`crate::state::drive`] flags a `sl goto` like a
/// `git checkout`. Fixed order (git then sl) keeps it stable. The Sapling
/// segment is only read when suppression is enabled. `None` only when neither
/// VCS contributes; a present-but-unreadable head contributes an empty segment
/// (the degraded `head_changed:false` path), not `None`.
fn read_head(v: &VcsDirs) -> Option<String> {
    let sl_active = v.sapling && v.sl_dir.is_some();
    if v.git_dir.is_none() && !sl_active {
        return None;
    }
    let git = v
        .git_dir
        .as_deref()
        .and_then(|d| std::fs::read_to_string(d.join("HEAD")).ok())
        .unwrap_or_default();
    let sl = if v.sapling {
        v.sl_dir
            .as_deref()
            .and_then(read_sl_parent)
            .unwrap_or_default()
    } else {
        String::new()
    };
    Some(format!("{git}|{sl}"))
}

/// First 20 bytes of `.sl/dirstate` = the working-copy parent (p1), hex-encoded
/// (manual hex avoids a `hex` dep). Read on the event loop like `.git/HEAD`;
/// `.sl/dirstate` is never *watched*, so a read-only `sl status` triggers no
/// read. Returns `None` on a non-regular (symlink/FIFO/…), short, or unreadable
/// file, so wrong/absent Sapling facts degrade to `head_changed:false` instead
/// of crashing — confirm the on-disk layout against the deployed Sapling version.
fn read_sl_parent(sl_dir: &Path) -> Option<String> {
    use std::io::Read;
    let dirstate = sl_dir.join("dirstate");
    // Reject non-regular files: a planted FIFO/symlink could block the
    // synchronous read on the event loop.
    if !std::fs::symlink_metadata(&dirstate)
        .ok()?
        .file_type()
        .is_file()
    {
        return None;
    }
    let mut f = std::fs::File::open(&dirstate).ok()?;
    let mut p1 = [0u8; 20];
    f.read_exact(&mut p1).ok()?;
    Some(p1.iter().map(|b| format!("{b:02x}")).collect())
}

async fn event_loop(
    mut raw_rx: mpsc::UnboundedReceiver<RawFsEvent>,
    out_tx: broadcast::Sender<FsEvent>,
    vcs: VcsDirs,
    cooldown: Duration,
    shutdown: CancellationToken,
) {
    let mut state = LockState::Idle;
    let mut stale_warn = StaleWarn::default();
    // Baseline for the next op's head_changed: the head last observed while
    // no op was running. Fast ops complete their whole lock cycle inside one
    // debounce batch, so the batch-time head is already post-op; this keeps
    // the pre-op value.
    let mut last_idle_head = read_head(&vcs);

    loop {
        let timer_deadline = match &state {
            LockState::Settling { until, .. } | LockState::Cooldown { until } => Some(*until),
            _ => None,
        };

        tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            maybe_event = raw_rx.recv() => {
                let Some(event) = maybe_event else { break };
                process_event(event, &mut state, &mut last_idle_head, &vcs, cooldown, &out_tx);
                if let Some(elapsed) = stale_warn.check(&state, Instant::now()) {
                    tracing::warn!("FsEventSource: VCS lock held for {elapsed:?}, treating as stale");
                }
            }
            _ = sleep_until_opt(timer_deadline) => {
                // Settle expiry emits the merged op's Completed; cooldown
                // expiry is usually silent Cooldown -> Idle. Either way drive
                // on fresh facts: a lock that reappeared during the wait
                // re-locks (Settling, silent) or emits Started (Cooldown) —
                // op entry uses the pre-op `last_idle_head` baseline exactly
                // like the event arm, so a mid-op head read can't become
                // `head_at_start`.
                let head_now = read_head(&vcs);
                let transition = if lock_present(&vcs) {
                    drive(&mut state, true, last_idle_head.clone(), Instant::now(), cooldown)
                } else {
                    drive(&mut state, false, head_now.clone(), Instant::now(), cooldown)
                };
                emit_transition(transition, &out_tx);
                if matches!(state, LockState::Idle | LockState::Cooldown { .. }) {
                    last_idle_head = head_now;
                }
            }
        }
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d.into()).await,
        None => std::future::pending::<()>().await,
    }
}

fn emit_transition(transition: LockTransition, out_tx: &broadcast::Sender<FsEvent>) {
    match transition {
        LockTransition::Started => {
            let _ = out_tx.send(FsEvent::GitOperationStarted);
        }
        LockTransition::Completed { head_changed } => {
            let _ = out_tx.send(FsEvent::GitOperationCompleted { head_changed });
        }
        LockTransition::None | LockTransition::CooldownEnded => {}
    }
}

fn process_event(
    raw: RawFsEvent,
    state: &mut LockState,
    last_idle_head: &mut Option<String>,
    vcs: &VcsDirs,
    cooldown: Duration,
    out_tx: &broadcast::Sender<FsEvent>,
) {
    // FS state read here, not at OS-event time. A fast git op can cycle its
    // lock entirely inside one debounce batch, so the lock file is already
    // gone when the batch is processed: treat a lock-path *event* as op
    // activity too, entering via the lock=true arm (Started / settle-merge)
    // and immediately releasing into Settling. Op entry records
    // `last_idle_head` — the batch-time head is already post-op for such
    // bursts — so the eventual Completed spans the real operation.
    let now = Instant::now();
    let head_now = read_head(vcs);
    let lock_now = lock_present(vcs);
    let saw_lock_event = raw.paths.iter().any(|p| vcs.is_lock_path(p));

    let transition = if lock_now || saw_lock_event {
        drive(state, true, last_idle_head.clone(), now, cooldown)
    } else {
        drive(state, false, head_now.clone(), now, cooldown)
    };
    emit_transition(transition, out_tx);
    if !lock_now && saw_lock_event {
        // Lock already gone: release into Settling (silent by construction).
        let transition = drive(state, false, head_now.clone(), now, cooldown);
        emit_transition(transition, out_tx);
    }
    // Accepted race: if a settle expires while the next op's lock event is
    // still in the debounce window, the baseline recorded here is already
    // that op's post-op head, so its Completed can read head_changed:false.
    // Self-healing: buffered FilesChanged force the consumer's rebuild, and
    // the hunk refresh has its own head_oid/index-mtime check.
    if matches!(state, LockState::Idle | LockState::Cooldown { .. }) {
        *last_idle_head = head_now;
    }

    // While in_op: suppress GitMetaChanged (one wake on Completed, not N).
    // Settling is in_op — the inter-cycle HEAD moves of a merged op must not
    // leak as meta wakes — but not in_cooldown: FilesChanged keeps flowing
    // during Locked/Settling (consumer buffers); Cooldown drops it.
    let in_op = matches!(
        state,
        LockState::Locked { .. } | LockState::Settling { .. } | LockState::Cooldown { .. }
    );
    let in_cooldown = matches!(state, LockState::Cooldown { .. });

    let mut file_paths: Vec<PathBuf> = Vec::with_capacity(raw.paths.len());
    let mut git_meta_kinds: Vec<GitMetaKind> = Vec::new();

    for path in raw.paths {
        match vcs.classify(&path) {
            Some(kind) => git_meta_kinds.push(kind),
            // VCS-internal (e.g. `*.lock`, any `.sl/*`): a state-machine tick,
            // not a workspace file. Drop.
            None if vcs.is_internal(&path) => {}
            None => file_paths.push(path),
        }
    }
    git_meta_kinds.sort();
    git_meta_kinds.dedup();

    if !in_op {
        for kind in git_meta_kinds {
            let _ = out_tx.send(FsEvent::GitMetaChanged { kind });
        }
    }
    if !in_cooldown && !file_paths.is_empty() {
        let _ = out_tx.send(FsEvent::FilesChanged {
            paths: file_paths,
            kind: raw.kind,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::FsEventKind;
    use crate::state::LockState;

    fn collect_events(rx: &mut broadcast::Receiver<FsEvent>) -> Vec<FsEvent> {
        let mut out = Vec::new();
        while let Ok(e) = rx.try_recv() {
            out.push(e);
        }
        out
    }

    fn raw(paths: Vec<&str>, kind: FsEventKind) -> RawFsEvent {
        RawFsEvent {
            paths: paths.into_iter().map(PathBuf::from).collect(),
            kind,
        }
    }

    fn cd() -> Duration {
        Duration::from_millis(500)
    }

    fn git_vcs(temp: &tempfile::TempDir) -> VcsDirs {
        VcsDirs {
            git_dir: Some(temp.path().join(".git")),
            sl_dir: None,
            sapling: true,
        }
    }

    fn sl_vcs(temp: &tempfile::TempDir) -> VcsDirs {
        VcsDirs {
            git_dir: None,
            sl_dir: Some(temp.path().join(".sl")),
            sapling: true,
        }
    }

    #[test]
    fn process_event_emits_files_changed_when_idle() {
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = None;
        process_event(
            raw(vec!["/r/src/main.rs"], FsEventKind::Modified),
            &mut state,
            &mut idle_head,
            &VcsDirs::default(),
            cd(),
            &out_tx,
        );
        let events = collect_events(&mut rx);
        assert_eq!(events.len(), 1);
        match &events[0] {
            FsEvent::FilesChanged { paths, kind } => {
                assert_eq!(paths.len(), 1);
                assert_eq!(*kind, FsEventKind::Modified);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn process_event_suppresses_git_meta_during_locked() {
        let temp = make_fake_git_repo_with_lock();
        let git_dir = temp.path().join(".git");

        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&git_vcs(&temp));
        process_event(
            raw(
                vec![git_dir.join("HEAD").to_str().unwrap()],
                FsEventKind::Modified,
            ),
            &mut state,
            &mut idle_head,
            &git_vcs(&temp),
            cd(),
            &out_tx,
        );

        let events = collect_events(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, FsEvent::GitOperationStarted))
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FsEvent::GitMetaChanged { .. }))
        );
    }

    #[test]
    fn process_event_emits_git_meta_when_idle() {
        let temp = make_fake_git_repo_no_lock();
        let git_dir = temp.path().join(".git");

        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&git_vcs(&temp));
        process_event(
            raw(
                vec![git_dir.join("HEAD").to_str().unwrap()],
                FsEventKind::Modified,
            ),
            &mut state,
            &mut idle_head,
            &git_vcs(&temp),
            cd(),
            &out_tx,
        );

        let events = collect_events(&mut rx);
        assert!(events.iter().any(|e| matches!(
            e,
            FsEvent::GitMetaChanged {
                kind: GitMetaKind::HeadChanged
            }
        )));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FsEvent::GitOperationStarted))
        );
    }

    #[test]
    fn process_event_drops_files_during_cooldown() {
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Cooldown {
            until: Instant::now() + Duration::from_millis(500),
        };
        let mut idle_head = None;
        process_event(
            raw(vec!["/r/src/main.rs"], FsEventKind::Modified),
            &mut state,
            &mut idle_head,
            &VcsDirs::default(),
            cd(),
            &out_tx,
        );
        let events = collect_events(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FsEvent::FilesChanged { .. }))
        );
    }

    #[test]
    fn process_event_drops_git_internal_paths_from_files_changed() {
        let temp = make_fake_git_repo_no_lock();
        let git_dir = temp.path().join(".git");
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&git_vcs(&temp));
        process_event(
            raw(
                vec![git_dir.join("index.lock").to_str().unwrap()],
                FsEventKind::Created,
            ),
            &mut state,
            &mut idle_head,
            &git_vcs(&temp),
            cd(),
            &out_tx,
        );
        let events = collect_events(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FsEvent::FilesChanged { .. })),
            ".git/index.lock must not surface as FilesChanged"
        );
    }

    fn make_fake_git_repo_no_lock() -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().unwrap();
        let git_dir = temp.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        temp
    }

    fn make_fake_git_repo_with_lock() -> tempfile::TempDir {
        let temp = make_fake_git_repo_no_lock();
        let git_dir = temp.path().join(".git");
        std::fs::write(git_dir.join("index.lock"), "").unwrap();
        temp
    }

    fn make_fake_git_repo_with_gc_pid() -> tempfile::TempDir {
        let temp = make_fake_git_repo_no_lock();
        let git_dir = temp.path().join(".git");
        std::fs::write(git_dir.join("gc.pid"), "").unwrap();
        temp
    }

    #[test]
    fn process_event_suppresses_git_meta_during_gc() {
        let temp = make_fake_git_repo_with_gc_pid();
        let git_dir = temp.path().join(".git");

        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&git_vcs(&temp));
        process_event(
            raw(
                vec![git_dir.join("packed-refs").to_str().unwrap()],
                FsEventKind::Modified,
            ),
            &mut state,
            &mut idle_head,
            &git_vcs(&temp),
            cd(),
            &out_tx,
        );

        let events = collect_events(&mut rx);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, FsEvent::GitOperationStarted)),
            "gc.pid should trigger GitOperationStarted"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FsEvent::GitMetaChanged { .. })),
            "GitMetaChanged must be suppressed while gc.pid is present"
        );
    }

    #[test]
    fn lock_present_detects_gc_pid() {
        let temp = make_fake_git_repo_with_gc_pid();
        assert!(lock_present(&git_vcs(&temp)));
    }

    #[test]
    fn lock_present_false_when_neither_lock_nor_gc_pid() {
        let temp = make_fake_git_repo_no_lock();
        assert!(!lock_present(&git_vcs(&temp)));
    }

    #[test]
    fn gc_pid_does_not_surface_as_files_changed() {
        let temp = make_fake_git_repo_no_lock();
        let git_dir = temp.path().join(".git");
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&git_vcs(&temp));
        process_event(
            raw(
                vec![git_dir.join("gc.pid").to_str().unwrap()],
                FsEventKind::Created,
            ),
            &mut state,
            &mut idle_head,
            &git_vcs(&temp),
            cd(),
            &out_tx,
        );
        let events = collect_events(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FsEvent::FilesChanged { .. })),
            ".git/gc.pid must not surface as FilesChanged"
        );
    }

    /// One rebase pick as the watcher sees it: `index.lock` appears, HEAD is
    /// rewritten while the lock is held, then the lock is released. Each FS
    /// change is fed through `process_event` like a real raw-event batch.
    fn simulate_rebase_pick(
        temp: &tempfile::TempDir,
        vcs: &VcsDirs,
        state: &mut LockState,
        idle_head: &mut Option<String>,
        out_tx: &broadcast::Sender<FsEvent>,
        pick: usize,
    ) {
        let git_dir = temp.path().join(".git");
        let lock = git_dir.join("index.lock");
        let head = git_dir.join("HEAD");

        std::fs::write(&lock, "").unwrap();
        process_event(
            raw(vec![lock.to_str().unwrap()], FsEventKind::Created),
            state,
            idle_head,
            vcs,
            cd(),
            out_tx,
        );

        std::fs::write(&head, format!("pick-{pick}\n")).unwrap();
        process_event(
            raw(vec![head.to_str().unwrap()], FsEventKind::Modified),
            state,
            idle_head,
            vcs,
            cd(),
            out_tx,
        );

        std::fs::remove_file(&lock).unwrap();
        process_event(
            raw(vec![lock.to_str().unwrap()], FsEventKind::Removed),
            state,
            idle_head,
            vcs,
            cd(),
            out_tx,
        );
    }

    #[test]
    fn rapid_lock_cycles_merge_into_one_operation() {
        let temp = make_fake_git_repo_no_lock();
        let vcs = git_vcs(&temp);
        let (out_tx, mut rx) = broadcast::channel(64);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&vcs);

        // Back-to-back picks: every re-lock lands inside the previous pick's
        // settle window, exercising the Settling -> Locked merge path.
        const PICKS: usize = 5;
        for pick in 0..PICKS {
            simulate_rebase_pick(&temp, &vcs, &mut state, &mut idle_head, &out_tx, pick);
        }

        // Mid-burst: only the first pick's Started so far; every later cycle
        // merged silently and the operation is still settling.
        assert_eq!(
            collect_events(&mut rx),
            vec![FsEvent::GitOperationStarted],
            "rapid cycles must not emit per-pick pairs"
        );

        expire_settle(&mut state, &mut idle_head, &vcs, &out_tx);

        // One Completed for the whole burst, spanning first pre-op HEAD
        // ("ref: refs/heads/main") to final HEAD ("pick-4").
        assert_eq!(
            collect_events(&mut rx),
            vec![FsEvent::GitOperationCompleted { head_changed: true }],
            "the merged operation completes exactly once"
        );
        assert!(matches!(state, LockState::Cooldown { .. }));
    }

    /// A fast pick completes its whole lock cycle inside one debounce batch:
    /// the lock file is already gone when the batch is processed, so op
    /// activity is inferred from the lock-path events. This is what real
    /// rebases look like on small repos (per-pick lock hold times are
    /// sub-debounce), so it is the storm's production shape.
    fn simulate_batched_pick(
        temp: &tempfile::TempDir,
        vcs: &VcsDirs,
        state: &mut LockState,
        idle_head: &mut Option<String>,
        out_tx: &broadcast::Sender<FsEvent>,
        pick: usize,
    ) {
        let git_dir = temp.path().join(".git");
        let head = git_dir.join("HEAD");
        std::fs::write(&head, format!("pick-{pick}\n")).unwrap();
        // One batch: lock created + HEAD rewritten + lock removed; no lock
        // file exists at processing time.
        process_event(
            raw(
                vec![
                    git_dir.join("index.lock").to_str().unwrap(),
                    head.to_str().unwrap(),
                ],
                FsEventKind::Modified,
            ),
            state,
            idle_head,
            vcs,
            cd(),
            out_tx,
        );
    }

    #[test]
    fn batched_lock_cycles_merge_into_one_operation() {
        let temp = make_fake_git_repo_no_lock();
        let vcs = git_vcs(&temp);
        let (out_tx, mut rx) = broadcast::channel(64);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&vcs);

        const PICKS: usize = 5;
        for pick in 0..PICKS {
            simulate_batched_pick(&temp, &vcs, &mut state, &mut idle_head, &out_tx, pick);
        }

        // One Started for the burst; per-pick HEAD moves are suppressed (the
        // op is settling, which counts as in-op) and each batch extends the
        // settle window instead of completing.
        assert_eq!(
            collect_events(&mut rx),
            vec![FsEvent::GitOperationStarted],
            "batched fast cycles must merge, not emit per-pick pairs or meta"
        );

        expire_settle(&mut state, &mut idle_head, &vcs, &out_tx);
        assert_eq!(
            collect_events(&mut rx),
            vec![FsEvent::GitOperationCompleted { head_changed: true }],
            "head comparison must span the merged op (pre-op head vs final)"
        );
    }

    /// A single fast op with no head move (e.g. `git add`): the batched lock
    /// cycle still produces a Started, and the settle expiry completes it
    /// with `head_changed: false`.
    #[test]
    fn batched_lock_cycle_without_head_move_completes_false() {
        let temp = make_fake_git_repo_no_lock();
        let vcs = git_vcs(&temp);
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&vcs);

        let git_dir = temp.path().join(".git");
        process_event(
            raw(
                vec![git_dir.join("index.lock").to_str().unwrap()],
                FsEventKind::Created,
            ),
            &mut state,
            &mut idle_head,
            &vcs,
            cd(),
            &out_tx,
        );
        assert_eq!(collect_events(&mut rx), vec![FsEvent::GitOperationStarted]);

        expire_settle(&mut state, &mut idle_head, &vcs, &out_tx);
        assert_eq!(
            collect_events(&mut rx),
            vec![FsEvent::GitOperationCompleted {
                head_changed: false
            }]
        );
        assert_eq!(state, LockState::Idle);
    }

    /// Only the `lock_present` trio (`index.lock`/`gc.pid`/`wlock`) may
    /// synthesize op activity: git cycles other transient `.git/*.lock`
    /// files (`config.lock`, `HEAD.lock`, per-ref locks) during non-op
    /// commands, and treating those as ops would open 500ms suppression
    /// windows around ordinary activity.
    #[test]
    fn non_op_lock_files_do_not_synthesize_ops() {
        let temp = make_fake_git_repo_no_lock();
        let vcs = git_vcs(&temp);
        let git_dir = temp.path().join(".git");
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&vcs);

        for name in ["config.lock", "HEAD.lock", "refs/heads/main.lock"] {
            process_event(
                raw(
                    vec![git_dir.join(name).to_str().unwrap()],
                    FsEventKind::Created,
                ),
                &mut state,
                &mut idle_head,
                &vcs,
                cd(),
                &out_tx,
            );
            assert_eq!(state, LockState::Idle, "{name} must not open an op");
        }
        assert!(
            !collect_events(&mut rx)
                .iter()
                .any(|e| matches!(e, FsEvent::GitOperationStarted)),
            "non-op lock files must never emit GitOperationStarted"
        );
    }

    // ========================================================================
    // Sapling (`.sl`) — the existing VCS-agnostic lock machine, fed `.sl` facts.
    // ========================================================================

    /// Two distinct 20-byte working-copy parents (p1) for head-change tests.
    const SL_P1_A: [u8; 20] = [0x11; 20];
    const SL_P1_B: [u8; 20] = [0x22; 20];

    /// Realistic-ish dirstate: `p1(20) ‖ p2(NULL_ID, 20) ‖ "\ntreestate\n…"`.
    /// `read_sl_parent` reads only the leading p1.
    fn fake_dirstate(p1: [u8; 20]) -> Vec<u8> {
        let mut v = Vec::with_capacity(48);
        v.extend_from_slice(&p1);
        v.extend_from_slice(&[0u8; 20]);
        v.extend_from_slice(b"\ntreestate\n");
        v
    }

    fn sl_hex(p1: [u8; 20]) -> String {
        p1.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn make_fake_sl_repo_no_lock() -> tempfile::TempDir {
        let temp = tempfile::TempDir::new().unwrap();
        let sl_dir = temp.path().join(".sl");
        std::fs::create_dir(&sl_dir).unwrap();
        std::fs::write(sl_dir.join("dirstate"), fake_dirstate(SL_P1_A)).unwrap();
        temp
    }

    fn make_fake_sl_repo_with_lock() -> tempfile::TempDir {
        let temp = make_fake_sl_repo_no_lock();
        std::fs::write(temp.path().join(".sl/wlock"), "").unwrap();
        temp
    }

    #[test]
    fn read_sl_parent_hex_zero_pads_low_bytes() {
        // Exactly 20 bytes (minimal valid input) of a sub-0x10 byte, checked
        // against a *literal* oracle independent of the production formula: a
        // dropped zero-pad would yield "a"×20, not "0a"×20.
        let temp = tempfile::TempDir::new().unwrap();
        let sl_dir = temp.path().join(".sl");
        std::fs::create_dir(&sl_dir).unwrap();
        std::fs::write(sl_dir.join("dirstate"), [0x0au8; 20]).unwrap();
        assert_eq!(
            read_sl_parent(&sl_dir).as_deref(),
            Some("0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a"),
        );
    }

    #[test]
    fn read_sl_parent_none_on_short_or_missing_dirstate() {
        let temp = tempfile::TempDir::new().unwrap();
        let sl_dir = temp.path().join(".sl");
        std::fs::create_dir(&sl_dir).unwrap();
        // Missing dirstate.
        assert_eq!(read_sl_parent(&sl_dir), None);
        // Boundary: 0 and 19 bytes must fail read_exact(20) → None (degrade).
        for len in [0usize, 19] {
            std::fs::write(sl_dir.join("dirstate"), vec![0u8; len]).unwrap();
            assert_eq!(read_sl_parent(&sl_dir), None, "{len}-byte dirstate → None");
        }
    }

    #[cfg(unix)]
    #[test]
    fn read_sl_parent_rejects_non_regular_dirstate() {
        // A symlinked dirstate (even to a valid 20-byte target) must be rejected
        // by the `is_file()` guard → None. Without the guard, `File::open` would
        // follow the link and read the target. The same guard rejects FIFOs,
        // which is what protects the event loop from a blocking read.
        let temp = tempfile::TempDir::new().unwrap();
        let sl_dir = temp.path().join(".sl");
        std::fs::create_dir(&sl_dir).unwrap();
        let target = temp.path().join("real_dirstate");
        std::fs::write(&target, [0x0au8; 20]).unwrap();
        std::os::unix::fs::symlink(&target, sl_dir.join("dirstate")).unwrap();
        assert_eq!(read_sl_parent(&sl_dir), None);
    }

    #[test]
    fn lock_present_detects_sl_wlock() {
        let locked = make_fake_sl_repo_with_lock();
        assert!(lock_present(&sl_vcs(&locked)), ".sl/wlock arms suppression");
        let unlocked = make_fake_sl_repo_no_lock();
        assert!(
            !lock_present(&sl_vcs(&unlocked)),
            "no .sl/wlock → not locked"
        );
    }

    #[test]
    fn read_head_combined_token_for_all_repo_shapes() {
        // pure git: "<HEAD>|"
        let git = make_fake_git_repo_no_lock();
        assert_eq!(
            read_head(&git_vcs(&git)),
            Some("ref: refs/heads/main\n|".to_string())
        );

        // pure Sapling: "|<p1>"
        let sl = make_fake_sl_repo_no_lock();
        assert_eq!(
            read_head(&sl_vcs(&sl)),
            Some(format!("|{}", sl_hex(SL_P1_A)))
        );

        // colocated `.sl`+`.git`: "<HEAD>|<p1>" — a move in either flips it.
        let both = tempfile::TempDir::new().unwrap();
        let git_dir = both.path().join(".git");
        std::fs::create_dir(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let sl_dir = both.path().join(".sl");
        std::fs::create_dir(&sl_dir).unwrap();
        std::fs::write(sl_dir.join("dirstate"), fake_dirstate(SL_P1_A)).unwrap();
        let colocated = VcsDirs {
            git_dir: Some(git_dir),
            sl_dir: Some(sl_dir),
            sapling: true,
        };
        assert_eq!(
            read_head(&colocated),
            Some(format!("ref: refs/heads/main\n|{}", sl_hex(SL_P1_A)))
        );

        // colocated, suppression off: the p1 segment is omitted even though the
        // dirstate is readable (Sapling reads are gated on `sapling`).
        let colocated_off = VcsDirs {
            git_dir: Some(both.path().join(".git")),
            sl_dir: Some(both.path().join(".sl")),
            sapling: false,
        };
        assert_eq!(
            read_head(&colocated_off),
            Some("ref: refs/heads/main\n|".to_string())
        );

        // neither: None (the no-repo behaviour).
        assert_eq!(read_head(&VcsDirs::default()), None);
        assert!(!lock_present(&VcsDirs::default()));

        // Degraded: a present `.sl` with an unreadable dirstate stays Some("|")
        // (head_changed:false path), not the no-repo None branch.
        let degraded = make_fake_sl_repo_no_lock();
        std::fs::remove_file(degraded.path().join(".sl/dirstate")).unwrap();
        assert_eq!(read_head(&sl_vcs(&degraded)), Some("|".to_string()));
    }

    #[test]
    fn sl_internal_paths_never_surface_or_emit_git_meta() {
        let temp = make_fake_sl_repo_no_lock();
        let sl_dir = temp.path().join(".sl");
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&sl_vcs(&temp));
        // Both the watched marker and a non-whitelisted internal file: neither
        // is a workspace file, neither is git metadata.
        for name in ["wlock", "dirstate", "store/lock"] {
            process_event(
                raw(
                    vec![sl_dir.join(name).to_str().unwrap()],
                    FsEventKind::Modified,
                ),
                &mut state,
                &mut idle_head,
                &sl_vcs(&temp),
                cd(),
                &out_tx,
            );
        }
        let events = collect_events(&mut rx);
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FsEvent::FilesChanged { .. })),
            ".sl/* must not surface as FilesChanged, got {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, FsEvent::GitMetaChanged { .. })),
            ".sl/* must never emit GitMetaChanged, got {events:?}"
        );
    }

    #[test]
    fn is_internal_anchors_to_discovered_sl_dir() {
        // The watch root's path contains an unrelated `.sl` ancestor while the
        // real repo's `.sl` is deeper. Internal-ness is anchored to the
        // discovered `sl_dir`, so a normal workspace file outside it is not
        // internal; only paths under the real `.sl` are.
        let vcs = VcsDirs {
            git_dir: None,
            sl_dir: Some(PathBuf::from("/x/.sl/proj/.sl")),
            sapling: true,
        };
        assert!(!vcs.is_internal(Path::new("/x/.sl/proj/src/main.rs")));
        assert!(vcs.is_internal(Path::new("/x/.sl/proj/.sl/wlock")));
    }

    #[test]
    fn workspace_file_surfaces_when_root_under_unrelated_sl_ancestor() {
        // A watch root under an unrelated `.sl` ancestor must not suppress its
        // workspace files: a normal file still surfaces as FilesChanged.
        let vcs = VcsDirs {
            git_dir: None,
            sl_dir: Some(PathBuf::from("/x/.sl/proj/.sl")),
            sapling: true,
        };
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&vcs);
        process_event(
            raw(vec!["/x/.sl/proj/src/main.rs"], FsEventKind::Modified),
            &mut state,
            &mut idle_head,
            &vcs,
            cd(),
            &out_tx,
        );
        assert!(
            collect_events(&mut rx)
                .iter()
                .any(|e| matches!(e, FsEvent::FilesChanged { .. })),
            "a normal file under a .sl ancestor must still surface"
        );
    }

    #[test]
    fn sl_internal_and_no_suppression_when_kill_switch_off() {
        // Kill-switch off: `sl_dir` is still discovered, so `.sl/*` stays
        // anchored-internal (never surfaces), yet suppression does not arm.
        let temp = make_fake_sl_repo_with_lock();
        let vcs = VcsDirs {
            git_dir: None,
            sl_dir: Some(temp.path().join(".sl")),
            sapling: false,
        };
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&sl_vcs(&temp));
        process_event(
            raw(
                vec![temp.path().join(".sl/wlock").to_str().unwrap()],
                FsEventKind::Created,
            ),
            &mut state,
            &mut idle_head,
            &vcs,
            cd(),
            &out_tx,
        );
        assert!(
            collect_events(&mut rx).is_empty(),
            ".sl/* must not leak, and suppression must not arm, when off"
        );
        assert_eq!(state, LockState::Idle, "kill-switch off → no suppression");
    }

    #[test]
    fn sl_status_no_op_dirstate_rewrite_emits_nothing() {
        // A read-only `sl status` may rewrite `.sl/dirstate` WITHOUT taking
        // `wlock`. dirstate is not whitelisted (read on demand), so such an
        // event is VCS-internal and there is no lock → no events at all.
        let temp = make_fake_sl_repo_no_lock();
        let sl_dir = temp.path().join(".sl");
        std::fs::write(sl_dir.join("dirstate"), fake_dirstate(SL_P1_B)).unwrap();
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&sl_vcs(&temp));
        process_event(
            raw(
                vec![sl_dir.join("dirstate").to_str().unwrap()],
                FsEventKind::Modified,
            ),
            &mut state,
            &mut idle_head,
            &sl_vcs(&temp),
            cd(),
            &out_tx,
        );
        assert!(
            collect_events(&mut rx).is_empty(),
            "a bare dirstate rewrite (no wlock) must emit no FsEvent"
        );
        assert_eq!(state, LockState::Idle, "state machine must stay Idle");
    }

    /// Drive the settle expiry exactly like the event loop's timer arm:
    /// re-read fresh facts at the deadline, drive with the pre-op baseline on
    /// re-lock, emit the transition, and maintain the idle-head baseline. The
    /// lock machine runs on std `Instant`, so tests expire the window by
    /// passing the deadline as `now` instead of sleeping.
    fn expire_settle(
        state: &mut LockState,
        idle_head: &mut Option<String>,
        vcs: &VcsDirs,
        out_tx: &broadcast::Sender<FsEvent>,
    ) {
        let until = match state {
            LockState::Settling { until, .. } => *until,
            other => panic!("expected Settling, got {other:?}"),
        };
        let head_now = read_head(vcs);
        let transition = if lock_present(vcs) {
            drive(state, true, idle_head.clone(), until, cd())
        } else {
            drive(state, false, head_now.clone(), until, cd())
        };
        emit_transition(transition, out_tx);
        if matches!(state, LockState::Idle | LockState::Cooldown { .. }) {
            *idle_head = head_now;
        }
    }

    /// Acquire `.sl/wlock` → move p1 in dirstate → release `wlock` yields a
    /// `Started → Completed{head_changed:true}` cycle (the `sl goto` win).
    #[test]
    fn sl_goto_cycle_reports_head_changed_true() {
        let temp = make_fake_sl_repo_no_lock();
        let vcs = sl_vcs(&temp);
        let wlock = temp.path().join(".sl/wlock");
        let dirstate = temp.path().join(".sl/dirstate");
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&vcs);

        std::fs::write(&wlock, "").unwrap();
        process_event(
            raw(vec![wlock.to_str().unwrap()], FsEventKind::Created),
            &mut state,
            &mut idle_head,
            &vcs,
            cd(),
            &out_tx,
        );
        // Exact: only Started (no stray FilesChanged from the wlock path).
        assert_eq!(collect_events(&mut rx), vec![FsEvent::GitOperationStarted]);

        // p1 moves while wlock is held, then wlock is released; Completed
        // arrives only once the settle window expires.
        std::fs::write(&dirstate, fake_dirstate(SL_P1_B)).unwrap();
        std::fs::remove_file(&wlock).unwrap();
        process_event(
            raw(vec![wlock.to_str().unwrap()], FsEventKind::Removed),
            &mut state,
            &mut idle_head,
            &vcs,
            cd(),
            &out_tx,
        );
        assert_eq!(collect_events(&mut rx), vec![]);
        expire_settle(&mut state, &mut idle_head, &vcs, &out_tx);
        assert_eq!(
            collect_events(&mut rx),
            vec![FsEvent::GitOperationCompleted { head_changed: true }]
        );
    }

    /// Same cycle but p1 unchanged (e.g. a dirty-treestate `sl status` that
    /// takes `wlock`): completes with `head_changed:false` (the degraded path).
    #[test]
    fn sl_lock_cycle_unchanged_p1_reports_head_changed_false() {
        let temp = make_fake_sl_repo_no_lock();
        let vcs = sl_vcs(&temp);
        let wlock = temp.path().join(".sl/wlock");
        let (out_tx, mut rx) = broadcast::channel(16);
        let mut state = LockState::Idle;
        let mut idle_head = read_head(&vcs);

        std::fs::write(&wlock, "").unwrap();
        process_event(
            raw(vec![wlock.to_str().unwrap()], FsEventKind::Created),
            &mut state,
            &mut idle_head,
            &vcs,
            cd(),
            &out_tx,
        );
        assert_eq!(collect_events(&mut rx), vec![FsEvent::GitOperationStarted]);

        // Release without moving p1; Completed after the settle window.
        std::fs::remove_file(&wlock).unwrap();
        process_event(
            raw(vec![wlock.to_str().unwrap()], FsEventKind::Removed),
            &mut state,
            &mut idle_head,
            &vcs,
            cd(),
            &out_tx,
        );
        assert_eq!(collect_events(&mut rx), vec![]);
        expire_settle(&mut state, &mut idle_head, &vcs, &out_tx);
        assert_eq!(
            collect_events(&mut rx),
            vec![FsEvent::GitOperationCompleted {
                head_changed: false
            }]
        );
    }

    #[test]
    fn discover_vcs_finds_sl_dir_regardless_but_gates_suppression() {
        // `sl_dir` is discovered either way (so `.sl/*` stays anchored-internal);
        // the `sapling` flag only arms suppression. No env mutation needed
        // (env→bool is covered by the watcher test).
        let temp = make_fake_sl_repo_with_lock();
        let on = discover_vcs(temp.path(), true);
        let off = discover_vcs(temp.path(), false);
        assert!(
            on.sl_dir.is_some() && off.sl_dir.is_some(),
            ".sl always found"
        );
        assert!(lock_present(&on), "wlock arms suppression when enabled");
        assert!(!lock_present(&off), "kill-switch off → no suppression");
    }
}
