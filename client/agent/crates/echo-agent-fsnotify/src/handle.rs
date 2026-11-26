//! The watcher's front door: config and strategy in, a running
//! [`FsNotifyHandle`] out. The thread it starts lives in [`crate::watcher`].
//!
//! Two OS-watch layouts (see [`WatchStrategy`]): fan-out watches each
//! non-ignored top-level child recursively, which is cheap where recursion is
//! kernel-side; per-dir watches one directory each, because inotify would
//! otherwise cost a descriptor per directory including ignored trees.
//!
//! Both skip the nested checkouts described in [`crate::checkout`], but only
//! per-dir skips them at any depth. Fan-out decides once per top-level child
//! and then hands the subtree to the OS, so a checkout below one it kept, or
//! any checkout at all once the root itself is watched recursively, stays
//! covered. Linux, where a watch costs a descriptor, uses per-dir.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::merge::RawFsEvent;
use crate::selection::build_globsets;
use crate::watcher::Watcher;

pub(crate) const DEBOUNCE_MS: u64 = 100;

/// Internal watcher config, behind the user-facing `crate::source::FsConfig`.
#[derive(Debug, Clone)]
pub(crate) struct FsNotifyConfig {
    pub debounce_ms: u64,
    pub ignore_patterns: Vec<String>,
}

impl Default for FsNotifyConfig {
    fn default() -> Self {
        Self {
            debounce_ms: DEBOUNCE_MS,
            ignore_patterns: vec![],
        }
    }
}

/// How OS watches are laid out over the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WatchStrategy {
    /// Recursive watches on top-level children, or on the root itself past
    /// [`crate::selection::MAX_TOP_LEVEL_FANOUT`].
    Fanout,
    /// One non-recursive watch per non-ignored directory, full depth.
    PerDir,
}

/// Per-dir on Linux and fan-out elsewhere, overridden by `ECHO_AGENT_FSNOTIFY_PER_DIR`.
pub(crate) fn watch_strategy() -> WatchStrategy {
    match std::env::var("ECHO_AGENT_FSNOTIFY_PER_DIR").ok().as_deref() {
        Some("1") | Some("true") => WatchStrategy::PerDir,
        Some("0") | Some("false") => WatchStrategy::Fanout,
        _ if cfg!(target_os = "linux") => WatchStrategy::PerDir,
        _ => WatchStrategy::Fanout,
    }
}

/// Per-dir mode's total watch budget (`ECHO_AGENT_FSNOTIFY_MAX_WATCHES` overrides).
const DEFAULT_MAX_WATCHES: usize = 49_152;

pub(crate) fn max_watch_budget() -> usize {
    std::env::var("ECHO_AGENT_FSNOTIFY_MAX_WATCHES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_WATCHES)
}

/// Work forwarded from notify's callback thread to the thread owning the debouncer.
pub(crate) enum WatchCommand {
    Reconcile,
    /// A structural delta. `pruned` is applied before `added` so a rename never
    /// unwatches the descriptor its new path just re-bound: inotify descriptors
    /// follow inodes.
    Update {
        pruned: Vec<std::path::PathBuf>,
        added: Vec<std::path::PathBuf>,
    },
    Shutdown,
}

pub(crate) struct FsNotifyHandle {
    cmd_tx: Option<std::sync::mpsc::Sender<WatchCommand>>,
    thread: Option<std::thread::JoinHandle<()>>,
    watch_count: Arc<AtomicUsize>,
}

impl FsNotifyHandle {
    pub(crate) fn watch_count(&self) -> usize {
        self.watch_count.load(Ordering::Relaxed)
    }
}

impl Drop for FsNotifyHandle {
    fn drop(&mut self) {
        tracing::debug!("fs_notify: stopping watcher thread");
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(WatchCommand::Shutdown);
        }
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }
}

const WATCHER_INIT_TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone)]
pub(crate) struct StartProgress {
    started_at: Instant,
    stage: &'static str,
    stage_started_at: Instant,
    timeline: Vec<(&'static str, Duration)>,
}

impl StartProgress {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            started_at: now,
            stage: "init",
            stage_started_at: now,
            timeline: vec![("init", Duration::from_millis(0))],
        }
    }

    pub(crate) fn set_stage(&mut self, stage: &'static str) {
        let t = self.started_at.elapsed();
        self.stage = stage;
        self.stage_started_at = Instant::now();
        if self.timeline.len() < 32 {
            self.timeline.push((stage, t));
        }
    }

    pub(crate) fn snapshot(
        &self,
    ) -> (
        &'static str,
        Duration,
        Duration,
        Vec<(&'static str, Duration)>,
    ) {
        (
            self.stage,
            self.stage_started_at.elapsed(),
            self.started_at.elapsed(),
            self.timeline.clone(),
        )
    }
}

pub(crate) fn start(
    watch_path: std::path::PathBuf,
    config: FsNotifyConfig,
    sapling: bool,
) -> Result<(mpsc::UnboundedReceiver<RawFsEvent>, FsNotifyHandle), crate::FsNotifyError> {
    start_with_timeout(
        watch_path,
        config,
        sapling,
        watch_strategy(),
        Duration::from_secs(WATCHER_INIT_TIMEOUT_SECS),
    )
}

/// Start with an explicit timeout and strategy, which tests pass directly.
pub(crate) fn start_with_timeout(
    watch_path: std::path::PathBuf,
    config: FsNotifyConfig,
    sapling: bool,
    strategy: WatchStrategy,
    init_timeout: Duration,
) -> Result<(mpsc::UnboundedReceiver<RawFsEvent>, FsNotifyHandle), crate::FsNotifyError> {
    let watch_path = dunce::canonicalize(&watch_path).unwrap_or(watch_path);
    tracing::debug!("fs_notify: starting watcher under {watch_path:?}");

    let (custom_ignore, custom_include) = build_globsets(&config.ignore_patterns);
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<WatchCommand>();
    let (ready_tx, ready_rx) =
        std::sync::mpsc::channel::<Result<(), Box<dyn std::error::Error + Send + Sync>>>();

    let watch_count = Arc::new(AtomicUsize::new(0));
    let progress = Arc::new(Mutex::new(StartProgress::new()));

    let watcher = Watcher {
        root: watch_path,
        strategy,
        sapling,
        debounce: Duration::from_millis(config.debounce_ms),
        custom_ignore: Arc::new(custom_ignore),
        custom_include: Arc::new(custom_include),
        budget: max_watch_budget(),
        events: events_tx,
        commands: cmd_tx.clone(),
        watch_count: Arc::clone(&watch_count),
        progress: Arc::clone(&progress),
    };

    set_stage(&progress, "spawning_watcher_thread");
    let thread = std::thread::Builder::new()
        .name("fsnotify-watcher".into())
        .spawn(move || watcher.run(cmd_rx, ready_tx))
        .map_err(|e| crate::FsNotifyError::WatcherStart(Box::new(e)))?;

    set_stage(&progress, "waiting_for_ready");
    match ready_rx.recv_timeout(init_timeout) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(crate::FsNotifyError::WatcherStart(e)),
        Err(_) => {
            report_timeout(&progress, init_timeout);
            let _ = cmd_tx.send(WatchCommand::Shutdown);
            return Err(crate::FsNotifyError::Timeout);
        }
    }

    Ok((
        events_rx,
        FsNotifyHandle {
            cmd_tx: Some(cmd_tx),
            thread: Some(thread),
            watch_count,
        },
    ))
}

fn set_stage(progress: &Arc<Mutex<StartProgress>>, stage: &'static str) {
    if let Ok(mut progress) = progress.lock() {
        progress.set_stage(stage);
    }
}

/// Names the phase a start that never signalled ready was sitting in, which is
/// the only clue a user gets when the connect timeout fires first.
fn report_timeout(progress: &Arc<Mutex<StartProgress>>, init_timeout: Duration) {
    let (stage, stage_elapsed, total_elapsed, timeline) =
        progress.lock().map(|p| p.snapshot()).unwrap_or((
            "unknown",
            Duration::from_secs(0),
            Duration::from_secs(0),
            Vec::new(),
        ));
    tracing::debug!(
        "watcher start timed out ({}s): stage={stage}, stage_elapsed={stage_elapsed:?}, \
         total_elapsed={total_elapsed:?}, timeline={timeline:?}",
        init_timeout.as_secs(),
    );
}
