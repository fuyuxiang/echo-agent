//! Session-level fs-watch policy over [`echo_agent_fsnotify`].
//!
//! Mechanism (OS watch, coalesce, refcount) lives in `echo_agent_fsnotify`. This module
//! decides which consumers exist, fans events through three explicit phases, and
//! owns one `select!` loop (event hot path + debounced refresh).

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol as acp;
use echo_agent_acp::AcpAgentGatewaySender as GatewaySender;
use echo_agent_fsnotify::{FsEvent, FsEventKind};
use echo_agent_hunk_tracker::HunkTrackerHandle;
use echo_agent_workspace::file_system::{CodebaseIndexManager, FileIndex, WalkOptions};
use tokio::sync::mpsc;
use tokio::time::sleep_until;

use crate::session::acp_session::SessionActor;
use crate::session::persistence::PersistenceMsg;
use crate::session::{ClientFsConfig, ClientFsMode};

// ── shared helpers ────────────────────────────────────────────────────────

/// True if `path` lies under a hidden component below `cwd`. Only the part
/// *below* `cwd` is inspected — a hidden ancestor of `cwd` itself is ignored.
pub(crate) fn is_under_hidden_dir(path: &Path, cwd: &Path) -> bool {
    let has_hidden = |rel: &Path| {
        rel.components().any(|c| {
            c.as_os_str()
                .to_str()
                .is_some_and(|s| s.starts_with('.') && s.len() > 1)
        })
    };
    if let Ok(rel) = path.strip_prefix(cwd) {
        return has_hidden(rel);
    }
    // Spelling mismatch (symlink/relative): retry on canonical roots before
    // giving up — never scan the whole absolute path.
    if let (Ok(p), Ok(c)) = (dunce::canonicalize(path), dunce::canonicalize(cwd))
        && let Ok(rel) = p.strip_prefix(&c)
    {
        return has_hidden(rel);
    }
    false
}

/// Forward fs event to hunk tracker, skipping hidden dirs.
pub(crate) fn forward_to_hunk_tracker(
    paths: &[PathBuf],
    kind: FsEventKind,
    handle: &HunkTrackerHandle,
    cwd: &Path,
) {
    for path in paths {
        if is_under_hidden_dir(path, cwd) {
            continue;
        }
        match kind {
            FsEventKind::Created | FsEventKind::Modified | FsEventKind::Renamed => {
                handle.handle_file_change(path.clone());
            }
            FsEventKind::Removed => {
                handle.handle_file_deleted(path.clone());
            }
            _ => {}
        }
    }
}

/// Dedup key for `echo.agent/git_head_changed`, shared by the watcher's `GitHead`
/// consumer and the post-edit `maybe_notify_git_branch` path so both compute
/// the same identity (branch | is_worktree | main_repo | commit). The commit
/// SHA is included so a same-branch commit (agent runs `git commit`) still
/// notifies clients — the changes panel must drop the now-committed files.
pub(crate) fn git_head_dedup_key(
    branch: Option<&str>,
    is_worktree: bool,
    main_repo: Option<&str>,
    commit: Option<&str>,
) -> String {
    // NUL separator: illegal in git refs and paths, so fields can't collide.
    format!(
        "{}\0{}\0{}\0{}",
        branch.unwrap_or(""),
        is_worktree,
        main_repo.unwrap_or(""),
        commit.unwrap_or("")
    )
}

/// Convert to codebase graph `FileEvent` for incremental index updates.
fn fs_event_to_codebase_graph_event(
    paths: &[PathBuf],
    kind: FsEventKind,
) -> echo_agent_codebase_graph::FileEvent {
    use echo_agent_codebase_graph::{FileEvent, FileEventKind};
    let kind = match kind {
        FsEventKind::Created => FileEventKind::Created,
        FsEventKind::Modified => FileEventKind::Modified,
        FsEventKind::Removed => FileEventKind::Removed,
        FsEventKind::Renamed => FileEventKind::Renamed,
        _ => FileEventKind::Modified,
    };
    FileEvent::new(paths.to_vec(), kind)
}

/// Convert to `FileIndexDelta` for TUI file tree updates.
fn fs_event_to_delta(
    paths: &[PathBuf],
    kind: FsEventKind,
    root: &Path,
) -> echo_agent_workspace::file_system::FileIndexDelta {
    use echo_agent_workspace::file_system::FileIndexDelta;
    let stripped: Vec<String> = paths
        .iter()
        .filter_map(|p| {
            if let Ok(rel) = p.strip_prefix(root) {
                return Some(rel.to_string_lossy().to_string());
            }
            if let (Ok(p_canon), Ok(root_canon)) =
                (dunce::canonicalize(p), dunce::canonicalize(root))
                && let Ok(rel) = p_canon.strip_prefix(&root_canon)
            {
                return Some(rel.to_string_lossy().to_string());
            }
            tracing::debug!("fs_event_to_delta: could not strip {:?} from {:?}", root, p);
            None
        })
        .filter(|p| !p.is_empty())
        .collect();

    match kind {
        FsEventKind::Created => {
            FileIndexDelta::Add(stripped.into_iter().map(|p| (p, false)).collect())
        }
        FsEventKind::Removed => FileIndexDelta::Remove(stripped),
        FsEventKind::Renamed => {
            if stripped.len() >= 2 {
                FileIndexDelta::Batch(vec![
                    FileIndexDelta::Remove(vec![stripped[0].clone()]),
                    FileIndexDelta::Add(vec![(stripped[1].clone(), false)]),
                ])
            } else if stripped.len() == 1 {
                FileIndexDelta::Add(stripped.into_iter().map(|p| (p, false)).collect())
            } else {
                FileIndexDelta::Batch(vec![])
            }
        }
        FsEventKind::Modified => FileIndexDelta::Batch(vec![]),
        _ => FileIndexDelta::Batch(vec![]),
    }
}

const GIT_DIFF_REBUILD_THRESHOLD: usize = 500;

fn parse_diff_name_status_line(
    line: &str,
    repo_root: &Path,
) -> Option<echo_agent_codebase_graph::FileEvent> {
    let mut parts = line.splitn(3, '\t');
    let status = parts.next()?.trim();
    let path = parts.next()?;

    match status.chars().next()? {
        'A' => Some(echo_agent_codebase_graph::FileEvent::created(
            repo_root.join(path),
        )),
        'D' => Some(echo_agent_codebase_graph::FileEvent::removed(
            repo_root.join(path),
        )),
        'R' | 'C' => {
            let new_path = parts.next()?;
            Some(echo_agent_codebase_graph::FileEvent::renamed(
                repo_root.join(path),
                repo_root.join(new_path),
            ))
        }
        _ => Some(echo_agent_codebase_graph::FileEvent::modified(
            repo_root.join(path),
        )),
    }
}

/// After a HEAD change, diff ORIG_HEAD..HEAD and send targeted events
/// to the codebase graph. Falls back to full rebuild if too many changes.
async fn refresh_codebase_graph_after_head_change(
    idx: &echo_agent_codebase_graph::IndexManagerHandle,
    repo_root: &Path,
) {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(["diff", "--name-status", "ORIG_HEAD", "HEAD"])
        .current_dir(repo_root)
        .stdin(std::process::Stdio::null());
    echo_agent_tools::util::detach_command(&mut cmd);
    cmd.envs(echo_agent_tools::util::pager_env());
    let diff_output = cmd.output().await;

    match diff_output {
        Ok(output) if output.status.success() => {
            let changed: Vec<_> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| parse_diff_name_status_line(l, repo_root))
                .collect();

            let count = changed.len();
            if count > GIT_DIFF_REBUILD_THRESHOLD {
                tracing::debug!(
                    "git_refresh: {count} changed files exceeds threshold, falling back to rebuild"
                );
                if let Err(e) = idx.rebuild() {
                    tracing::debug!("git_refresh: rebuild failed: {:?}", e);
                }
            } else if let Err(e) = idx.send_events(changed) {
                tracing::debug!("git_refresh: failed to send graph events: {:?}", e);
            } else {
                tracing::debug!("git_refresh: sent {count} changed files to codebase graph");
            }
        }
        _ => {
            tracing::debug!("git_refresh: git diff failed, falling back to rebuild");
            if let Err(e) = idx.rebuild() {
                tracing::debug!("git_refresh: rebuild fallback also failed: {:?}", e);
            }
        }
    }
}

// ── capabilities / deps / plan ────────────────────────────────────────────

/// What the client wants from the watcher: pure, `Copy`, mode-independent.
/// Proxy/hub gating of the actual spawn lives at the call site, not here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FsWatchCapabilities {
    pub client_notify: bool,
    pub hunk_tracking: bool,
    pub code_nav: bool,
    pub git_head: bool,
}

impl FsWatchCapabilities {
    /// True if any consumer needs the watcher.
    pub(crate) fn needs_watcher(self) -> bool {
        self.client_notify || self.hunk_tracking || self.code_nav || self.git_head
    }

    pub(crate) fn none() -> Self {
        Self::default()
    }

    /// Resolve the client's advertised signals into capabilities.
    pub(crate) fn resolve(inputs: CapabilityInputs) -> Self {
        Self {
            client_notify: inputs.client_notify,
            hunk_tracking: inputs.hunk_tracking,
            code_nav: inputs.code_nav,
            git_head: inputs.git_head_changed.unwrap_or(false),
        }
    }
}

/// Per-consumer signals fed to [`FsWatchCapabilities::resolve`]. Named fields
/// (not positional bools) so call sites can't silently transpose them.
pub(crate) struct CapabilityInputs {
    pub client_notify: bool,
    pub hunk_tracking: bool,
    pub code_nav: bool,
    /// `echo.agent/gitHeadChanged`; opt-in (absent => off).
    pub git_head_changed: Option<bool>,
}

/// Handles gathered only when actually building consumers.
pub(crate) struct FsWatchDeps {
    pub gateway: GatewaySender,
    pub session_id: String,
    pub cwd: PathBuf,
    pub index_root: PathBuf,
    pub hunk_tracker: HunkTrackerHandle,
    pub hunk_tracking_enabled: bool,
    pub codebase_indexes: Arc<parking_lot::Mutex<CodebaseIndexManager>>,
    pub client_fs_config: Option<ClientFsConfig>,
    pub persistence_tx: mpsc::UnboundedSender<PersistenceMsg>,
    pub last_reported_branch: Arc<parking_lot::Mutex<Option<String>>>,
    /// A signal, not an entry point: `run_status_emitter` is the only consumer
    /// and decides whether the row is worth building.
    pub status_wake: Arc<tokio::sync::Notify>,
}

impl FsWatchDeps {
    pub(crate) fn from_session(
        session: &SessionActor,
        client_fs_config: Option<ClientFsConfig>,
        codebase_indexes: Arc<parking_lot::Mutex<CodebaseIndexManager>>,
        index_root: PathBuf,
    ) -> Self {
        Self {
            gateway: session.notifications.gateway.clone(),
            session_id: session.session_info.id.to_string(),
            cwd: session.tool_context.cwd.to_path_buf(),
            index_root,
            hunk_tracker: session.tool_context.hunk_tracker_handle.clone(),
            hunk_tracking_enabled: session.tool_context.hunk_tracking_enabled,
            codebase_indexes,
            client_fs_config,
            persistence_tx: session.notifications.persistence_tx.clone(),
            last_reported_branch: session.last_reported_branch.clone(),
            status_wake: session.status_wake.handle(),
        }
    }
}

// ── consumers (private, async, !Send — session LocalSet) ──────────────────

struct ClientNotify {
    gateway: GatewaySender,
    session_id: String,
    cwd: PathBuf,
    mode: ClientFsMode,
}

impl ClientNotify {
    async fn on_change(&self, paths: &[PathBuf], kind: FsEventKind) {
        use serde_json::value::to_raw_value;

        match self.mode {
            ClientFsMode::Events => {
                // Present-tense strings are the `echo.agent/fs_notify` wire protocol;
                // do not sync to internal variant names.
                let kind_str = match kind {
                    FsEventKind::Created => "Create",
                    FsEventKind::Modified => "Modify",
                    FsEventKind::Removed => "Remove",
                    FsEventKind::Renamed => "Rename",
                    _ => "Other",
                };
                let path_strs: Vec<String> = paths
                    .iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect();
                let params = serde_json::json!({
                    "sessionId": self.session_id,
                    "event": { "kind": kind_str, "paths": path_strs },
                });
                if let Ok(raw) = to_raw_value(&params) {
                    self.gateway
                        .forward_fire_and_forget(acp::ExtNotification::new(
                            "echo.agent/fs_notify",
                            raw.into(),
                        ));
                }
            }
            ClientFsMode::Index => {
                if kind == FsEventKind::Modified {
                    return;
                }
                let delta = fs_event_to_delta(paths, kind, &self.cwd);
                tracing::debug!(
                    "fs_notify delta for {:?}: {:?}, is_empty={}",
                    kind,
                    delta.to_json(),
                    delta.is_empty()
                );
                if delta.is_empty() {
                    return;
                }
                let params = serde_json::json!({
                    "sessionId": self.session_id,
                    "delta": delta.to_json(),
                });
                if let Ok(raw) = to_raw_value(&params) {
                    self.gateway
                        .forward_fire_and_forget(acp::ExtNotification::new(
                            "echo.agent/fs/index/delta",
                            raw.into(),
                        ));
                }
            }
        }
    }

    async fn send_initial_file_index(&self) {
        const FILE_INDEX_CHUNK_SIZE: usize = 500;
        let session_id = &self.session_id;
        let cwd = &self.cwd;

        let (index_res, build_elapsed_ms) =
            crate::timed!({ FileIndex::from_walk_with_options(cwd, WalkOptions::default()) });
        let index = match index_res {
            Ok(idx) => idx,
            Err(e) => {
                tracing::warn!("failed to build file index for {:?}: {:?}", cwd, e);
                return;
            }
        };

        let total_entries = index.len();
        tracing::info!(
            "Built file index with {} entries in {}ms",
            total_entries,
            build_elapsed_ms
        );

        let entries: Vec<_> = index.iter().collect();
        let total_chunks = entries.len().div_ceil(FILE_INDEX_CHUNK_SIZE);

        let (_, send_elapsed_ms) = crate::timed!({
            for (chunk_idx, chunk) in entries.chunks(FILE_INDEX_CHUNK_SIZE).enumerate() {
                let is_complete = chunk_idx == total_chunks - 1;

                let files: Vec<serde_json::Value> = chunk
                    .iter()
                    .map(|(path, is_dir)| {
                        serde_json::json!({
                            "path": path,
                            "isDir": is_dir
                        })
                    })
                    .collect();

                let params = serde_json::json!({
                    "sessionId": session_id,
                    "root": cwd.to_string_lossy(),
                    "files": files,
                    "chunk": chunk_idx,
                    "totalChunks": total_chunks,
                    "totalFiles": total_entries,
                    "complete": is_complete,
                });

                if let Ok(raw) = serde_json::value::to_raw_value(&params) {
                    self.gateway
                        .forward_fire_and_forget(acp::ExtNotification::new(
                            "echo.agent/fs/index",
                            raw.into(),
                        ));
                }

                if !is_complete {
                    tokio::task::yield_now().await;
                }
            }
        });

        tracing::info!(
            "Sent file index ({} entries in {} chunks) in {}ms",
            total_entries,
            total_chunks,
            send_elapsed_ms
        );
    }
}

#[derive(Clone)]
struct HunkTracking {
    handle: HunkTrackerHandle,
    cwd: PathBuf,
    /// Live per-event forward needs a git root (baselines are HEAD-relative).
    git_root: Option<PathBuf>,
}

impl HunkTracking {
    fn on_change(&self, paths: &[PathBuf], kind: FsEventKind) {
        if self.git_root.is_none() {
            return;
        }
        forward_to_hunk_tracker(paths, kind, &self.handle, &self.cwd);
    }

    fn reconcile(&self) {
        self.handle.refresh_all_baselines();
    }
}

#[derive(Clone)]
struct CodebaseIndex {
    indexes: Arc<parking_lot::Mutex<CodebaseIndexManager>>,
    root: PathBuf,
}

impl CodebaseIndex {
    fn on_change(&self, paths: &[PathBuf], kind: FsEventKind) {
        if let Some(idx) = self.indexes.lock().get(&self.root) {
            let _ = idx.send_event(fs_event_to_codebase_graph_event(paths, kind));
        }
    }

    async fn rebuild_after_head_change(&self) {
        let idx = self.indexes.lock().get(&self.root);
        if let Some(idx) = idx {
            refresh_codebase_graph_after_head_change(&idx, &self.root).await;
        }
    }
}

#[derive(Clone)]
struct GitHead {
    gateway: GatewaySender,
    session_id: String,
    cwd: PathBuf,
    persistence_tx: mpsc::UnboundedSender<PersistenceMsg>,
    /// Dedup slot shared with `SessionActor::maybe_notify_git_branch` (see
    /// `git_head_dedup_key`).
    last: Arc<parking_lot::Mutex<Option<String>>>,
    status_wake: Arc<tokio::sync::Notify>,
}

impl GitHead {
    /// Notify the client on branch/worktree/repo change; also persist commit/branch.
    async fn emit(&self) {
        let branch = echo_agent_workspace::session::git::get_branch(&self.cwd).await;
        let commit = echo_agent_workspace::session::git::get_current_commit(&self.cwd).await;
        let worktree = echo_agent_workspace::session::git::get_worktree_info(&self.cwd).await;
        let (is_worktree, main_repo) = worktree.unwrap_or((false, None));

        let dedup_key = git_head_dedup_key(
            branch.as_deref(),
            is_worktree,
            main_repo.as_deref(),
            commit.as_deref(),
        );
        let changed = {
            let mut last = self.last.lock();
            if last.as_deref() == Some(&dedup_key) {
                false
            } else {
                *last = Some(dedup_key);
                true
            }
        };
        if changed {
            let params = echo_agent_workspace::session::git::GitHeadChanged {
                session_id: self.session_id.clone(),
                branch: branch.clone(),
                is_worktree,
                main_repo,
            };
            if let Ok(raw) = serde_json::value::to_raw_value(&params) {
                self.gateway
                    .forward_fire_and_forget(acp::ExtNotification::new(
                        "echo.agent/git_head_changed",
                        raw.into(),
                    ));
            }
            // The snapshot carries `workspace.branch`, so a checkout between
            // turns has to repush it or the row names the old branch until the
            // next turn ends.
            self.status_wake.notify_one();
        }

        let _ = self
            .persistence_tx
            .send(PersistenceMsg::GitHead { commit, branch });
    }
}

// ── plan ──────────────────────────────────────────────────────────────────

/// Live consumer set. Built only when `needs_watcher()`.
pub(crate) struct FsWatchPlan {
    client_notify: Option<ClientNotify>,
    hunk: Option<HunkTracking>,
    index: Option<CodebaseIndex>,
    git_head: Option<GitHead>,
    fs_config: echo_agent_fsnotify::FsConfig,
    cwd: PathBuf,
}

impl FsWatchPlan {
    pub(crate) fn build(caps: FsWatchCapabilities, deps: FsWatchDeps) -> Self {
        let cfg = deps.client_fs_config.unwrap_or_default();
        let mode = cfg.mode;
        let fs_config = cfg.fs;

        let client_notify = caps.client_notify.then(|| ClientNotify {
            gateway: deps.gateway.clone(),
            session_id: deps.session_id.clone(),
            cwd: deps.cwd.clone(),
            mode,
        });

        let hunk = (caps.hunk_tracking && deps.hunk_tracking_enabled).then(|| {
            let git_root =
                echo_agent_workspace::session::git::find_git_root_from_path(&deps.cwd).ok();
            HunkTracking {
                handle: deps.hunk_tracker,
                cwd: deps.cwd.clone(),
                git_root,
            }
        });

        // Index handle looked up per-event so a later-created index still receives events.
        let index = caps.code_nav.then(|| CodebaseIndex {
            indexes: deps.codebase_indexes,
            root: deps.index_root,
        });

        // Built only when the client opted into git-head notifications. The
        // restore-code consumer (the pager) advertises it, so it persists HEAD
        // here on every refresh; the end-of-turn `PersistGitHead` is a
        // best-effort fallback (trace-gated), not the load-bearing path.
        let git_head = caps.git_head.then(|| GitHead {
            gateway: deps.gateway,
            session_id: deps.session_id,
            cwd: deps.cwd.clone(),
            persistence_tx: deps.persistence_tx,
            last: deps.last_reported_branch,
            status_wake: deps.status_wake,
        });

        Self {
            client_notify,
            hunk,
            index,
            git_head,
            fs_config,
            cwd: deps.cwd,
        }
    }

    // Phase subsets differ on purpose — replay excludes hunk (baselines on refresh).

    async fn on_files_changed(&self, paths: &[PathBuf], kind: FsEventKind) {
        if let Some(h) = &self.hunk {
            h.on_change(paths, kind);
        }
        if let Some(i) = &self.index {
            i.on_change(paths, kind);
        }
        if let Some(c) = &self.client_notify {
            c.on_change(paths, kind).await;
        }
    }

    async fn on_replayed_change(&self, paths: &[PathBuf], kind: FsEventKind) {
        if let Some(i) = &self.index {
            i.on_change(paths, kind);
        }
        if let Some(c) = &self.client_notify {
            c.on_change(paths, kind).await;
        }
    }

    /// A self-contained refresh (clones the handles it needs) so it runs on its
    /// own `spawn_local` without blocking the event loop's `recv`.
    fn refresh_future(
        &self,
        rebuild_index: bool,
    ) -> impl std::future::Future<Output = ()> + 'static {
        let hunk = self.hunk.clone();
        let index = self.index.clone();
        let git_head = self.git_head.clone();
        async move {
            if let Some(h) = &hunk {
                h.reconcile();
            }
            if rebuild_index && let Some(i) = &index {
                i.rebuild_after_head_change().await;
            }
            if let Some(g) = &git_head {
                g.emit().await;
            }
        }
    }
}

// ── op buffer / debounce ──────────────────────────────────────────────────

type FsBatch = (Vec<PathBuf>, FsEventKind);

/// Resolve a completed git op: report the batches to replay plus whether a
/// codebase-graph rebuild is needed. Occupancy is sampled *before* the buffer
/// is drained, so an EdenFS-degraded `goto` (`head_changed: false` but a
/// replayed flood) still requests the rebuild.
fn resolve_completed_op(head_changed: bool, op_buffer: &mut Vec<FsBatch>) -> (Vec<FsBatch>, bool) {
    let had_buffered = !op_buffer.is_empty();
    let replay = if head_changed {
        op_buffer.clear();
        Vec::new()
    } else {
        std::mem::take(op_buffer)
    };
    (replay, head_changed || had_buffered)
}

/// Cap on batches buffered during a git op; exceeding it means the completion
/// boundary was lost (crashed git / stale `.git` lock) or a settle-merged op
/// is bigger than we are willing to buffer and replay, so recover with a
/// rebuild refresh instead of buffering forever.
const MAX_OP_BUFFER: usize = 10_000;

/// What the loop should do with one fs event. Pure (no I/O, no timers) so the
/// `in_op`/`op_buffer` state machine is unit-testable; the loop performs the
/// async consumer work and owns the debounce timer.
enum Outcome {
    /// Op started, event buffered, or ignored — nothing to do now.
    Buffered,
    /// Forward a live change to the consumers.
    Forward(FsBatch),
    /// Replay these batches, then refresh (rebuild the graph if set).
    Completed { replay: Vec<FsBatch>, rebuild: bool },
    /// Refresh only (rebuild if set) — git-metadata change or a resync.
    Refresh { rebuild: bool },
}

fn on_event(ev: FsEvent, in_op: &mut bool, op_buffer: &mut Vec<FsBatch>) -> Outcome {
    match ev {
        FsEvent::GitOperationStarted => {
            *in_op = true;
            Outcome::Buffered
        }
        FsEvent::GitOperationCompleted { head_changed } => {
            *in_op = false;
            let (replay, rebuild) = resolve_completed_op(head_changed, op_buffer);
            Outcome::Completed { replay, rebuild }
        }
        FsEvent::FilesChanged { paths, kind } if *in_op => {
            if op_buffer.len() >= MAX_OP_BUFFER {
                op_buffer.clear();
                *in_op = false;
                Outcome::Refresh { rebuild: true }
            } else {
                op_buffer.push((paths, kind));
                Outcome::Buffered
            }
        }
        FsEvent::FilesChanged { paths, kind } => Outcome::Forward((paths, kind)),
        FsEvent::GitMetaChanged { .. } => Outcome::Refresh { rebuild: true },
        _ => Outcome::Buffered,
    }
}

/// Recovery after a dropped broadcast event (`Lagged`): a missed GitOperation
/// boundary could leave `in_op` stuck and `op_buffer` orphaned.
fn on_resync(in_op: &mut bool, op_buffer: &mut Vec<FsBatch>) -> Outcome {
    *in_op = false;
    op_buffer.clear();
    Outcome::Refresh { rebuild: true }
}

const REFRESH_DEBOUNCE_QUIET: Duration = Duration::from_millis(500);
const REFRESH_DEBOUNCE_MAX_WAIT: Duration = Duration::from_secs(3);

/// Debounce state owned by the watcher loop (single task, no locking).
struct Debounce {
    /// Cap from first event in the burst.
    max_deadline: Option<tokio::time::Instant>,
    /// Quiet window; reset on each mid-burst notify.
    quiet_deadline: Option<tokio::time::Instant>,
}

impl Debounce {
    fn idle() -> Self {
        Self {
            max_deadline: None,
            quiet_deadline: None,
        }
    }

    fn active(&self) -> bool {
        self.max_deadline.is_some()
    }

    /// Earliest of the quiet window and the max-wait cap — whichever fires first.
    fn next_deadline(&self) -> Option<tokio::time::Instant> {
        match (self.quiet_deadline, self.max_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Extend the quiet window, or start a fresh debounce if idle.
    fn bump(&mut self) {
        if self.active() {
            self.quiet_deadline = Some(tokio::time::Instant::now() + REFRESH_DEBOUNCE_QUIET);
        } else {
            self.arm();
        }
    }

    fn arm(&mut self) {
        let now = tokio::time::Instant::now();
        self.max_deadline = Some(now + REFRESH_DEBOUNCE_MAX_WAIT);
        self.quiet_deadline = Some(now + REFRESH_DEBOUNCE_QUIET);
    }

    fn clear(&mut self) {
        self.max_deadline = None;
        self.quiet_deadline = None;
    }
}

/// Give-up bound on consecutive in-op deferrals: at the 500ms quiet cadence
/// this is ~60s, matching the watcher's stale-lock threshold. A crashed git
/// leaving `.git/index.lock` parks the source in its locked state forever
/// (no Completed, no resync, buffer never overflows on a quiet workspace),
/// so without a bound the deferral would starve refreshes indefinitely; one
/// forced fire per minute also caps the cost on pathologically long real ops.
const MAX_CONSECUTIVE_IN_OP_DEFERS: u32 = 120;

/// Decision for a due debounce deadline. Split out of the select loop's
/// settle arm (pure bookkeeping, no I/O) so tests drive the real logic.
enum SettleAction {
    /// Run the refresh now, with the accumulated rebuild request.
    Fire { rebuild: bool },
    /// Not now — re-armed to retry after the next quiet window.
    Defer,
}

/// The settle arm's decision: defer while a git op is in flight (a debounce
/// armed by pre-op edits or meta changes would otherwise scan a mid-op
/// worktree; Completed bumps the window afterwards anyway) or while the
/// single-flight refresh is still running (coalesce into the next pass);
/// otherwise fire, taking the rebuild flag. In-op deferrals are bounded by
/// [`MAX_CONSECUTIVE_IN_OP_DEFERS`] so a wedged op (stale lock file) cannot
/// starve refreshes forever.
fn on_settle_due(
    in_op: bool,
    refreshing: bool,
    debounce: &mut Debounce,
    rebuild_flag: &mut bool,
    in_op_defers: &mut u32,
) -> SettleAction {
    debounce.clear();
    if refreshing {
        // Single-flight is self-limiting (the running refresh completes), so
        // it never counts toward the give-up bound.
        debounce.arm();
        return SettleAction::Defer;
    }
    if in_op && *in_op_defers < MAX_CONSECUTIVE_IN_OP_DEFERS {
        *in_op_defers += 1;
        // Deferral re-arms in full, intentionally resetting the max-wait cap:
        // every deferral means a refresh is already owed and retried each
        // quiet window, so the cap's job (bounding un-fired bursts) is moot
        // here; the give-up bound above owns wedged-op starvation instead.
        debounce.arm();
        return SettleAction::Defer;
    }
    *in_op_defers = 0;
    SettleAction::Fire {
        rebuild: std::mem::take(rebuild_flag),
    }
}

/// Bookkeeping shared by the `Completed`/`Refresh` outcome arms: record the
/// rebuild request and (re)start the debounce quiet window.
fn note_refresh_request(rebuild: bool, debounce: &mut Debounce, rebuild_flag: &mut bool) {
    *rebuild_flag |= rebuild;
    debounce.bump();
}

// ── spawn / handle ────────────────────────────────────────────────────────

/// Resets the single-flight flag on drop (covers normal completion and panic).
struct ResetOnDrop(Rc<Cell<bool>>);
impl Drop for ResetOnDrop {
    fn drop(&mut self) {
        self.0.set(false);
    }
}

/// RAII drop-guard: dropping closes the shutdown channel and cancels the loop.
pub(crate) struct FsWatchHandle {
    _shutdown_tx: mpsc::UnboundedSender<()>,
}

/// Spawn the watcher task; caller holds the handle for the session lifetime.
pub(crate) fn spawn(plan: FsWatchPlan) -> FsWatchHandle {
    let (shutdown_tx, mut shutdown_rx) = mpsc::unbounded_channel::<()>();
    let cwd = plan.cwd.clone();
    let fs_config = plan.fs_config.clone();
    let send_index_at_start = plan
        .client_notify
        .as_ref()
        .is_some_and(|c| c.mode == ClientFsMode::Index);

    tracing::debug!(
        ?cwd,
        client_notify = plan.client_notify.is_some(),
        "fs-notify: starting FsEventSource"
    );

    tokio::task::spawn_local(async move {
        tokio::task::yield_now().await;
        let start_time = std::time::Instant::now();

        let source = {
            let mut timer = crate::instrumentation_timer!("session.fs_notify_start");
            timer.with_field("cwd", cwd.to_string_lossy().as_ref());
            let init_cwd = cwd.clone();
            let result = tokio::task::spawn_blocking(move || {
                echo_agent_fsnotify::shared(init_cwd, fs_config)
            })
            .await;
            let ws = echo_agent_fsnotify::stats();
            timer.with_field("live_watchers", ws.live_watchers as u64);
            timer.with_field("watchers_created_total", ws.created_total);
            timer.with_field("watchers_reused_total", ws.reused_total);
            result
        };
        let source = match source {
            Ok(Ok(s)) => {
                tracing::debug!("FsEventSource ready in {:?}", start_time.elapsed());
                s
            }
            Ok(Err(e)) => {
                tracing::warn!("failed to start FsEventSource: {e:?}");
                return;
            }
            Err(join_err) => {
                tracing::warn!("FsEventSource init task failed: {join_err:?}");
                return;
            }
        };

        let mut events = source.subscribe();
        if send_index_at_start && let Some(c) = &plan.client_notify {
            c.send_initial_file_index().await;
        }

        let mut rebuild_codebase_graph = false;
        let mut in_op = false;
        let mut op_buffer: Vec<FsBatch> = Vec::new();
        let mut debounce = Debounce::idle();
        let mut in_op_defers = 0u32;
        // Single-flight: refresh runs on its own task so the loop keeps draining
        // events; a settle while one is in flight re-arms instead of overlapping.
        let refreshing = Rc::new(Cell::new(false));

        loop {
            let settle = async {
                match debounce.next_deadline() {
                    Some(d) => sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };

            tokio::select! {
                biased;
                _ = shutdown_rx.recv() => {
                    tracing::debug!("fs_notify: shutdown");
                    break;
                }
                _ = settle, if debounce.active() => {
                    match on_settle_due(
                        in_op,
                        refreshing.get(),
                        &mut debounce,
                        &mut rebuild_codebase_graph,
                        &mut in_op_defers,
                    ) {
                        SettleAction::Defer => {}
                        SettleAction::Fire { rebuild } => {
                            refreshing.set(true);
                            let fut = plan.refresh_future(rebuild);
                            let reset = ResetOnDrop(refreshing.clone());
                            tokio::task::spawn_local(async move {
                                // Reset on completion OR panic so a panicking refresh
                                // can't wedge single-flight for the session's life.
                                let _reset = reset;
                                fut.await;
                            });
                        }
                    }
                }
                ev_result = events.recv() => {
                    let outcome = match ev_result {
                        Ok(ev) => on_event(ev, &mut in_op, &mut op_buffer),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("fs_events: dropped {n} (consumer lagged); resyncing");
                            on_resync(&mut in_op, &mut op_buffer)
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    match outcome {
                        Outcome::Buffered => {}
                        Outcome::Forward((paths, kind)) => {
                            plan.on_files_changed(&paths, kind).await;
                        }
                        Outcome::Completed { replay, rebuild } => {
                            for (paths, kind) in replay {
                                plan.on_replayed_change(&paths, kind).await;
                            }
                            // Always refresh on completion: a same-branch commit
                            // moves HEAD with no rebuild, but branch/commit still
                            // need the pass.
                            note_refresh_request(rebuild, &mut debounce, &mut rebuild_codebase_graph);
                        }
                        Outcome::Refresh { rebuild } => {
                            note_refresh_request(rebuild, &mut debounce, &mut rebuild_codebase_graph);
                        }
                    }
                }
            }
        }
    });

    FsWatchHandle {
        _shutdown_tx: shutdown_tx,
    }
}

// ── tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use echo_agent_workspace::file_system::FileIndexDelta;
    use std::path::PathBuf;

    #[test]
    fn fs_event_to_delta_create() {
        let root = PathBuf::from("/workspace");
        let delta = fs_event_to_delta(
            &[PathBuf::from("/workspace/src/main.rs")],
            FsEventKind::Created,
            &root,
        );
        match delta {
            FileIndexDelta::Add(entries) => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].0, "src/main.rs");
            }
            _ => panic!("Expected Add delta, got {:?}", delta),
        }
    }

    #[test]
    fn fs_event_to_delta_rename_pair() {
        let root = PathBuf::from("/workspace");
        let delta = fs_event_to_delta(
            &[
                PathBuf::from("/workspace/old.rs"),
                PathBuf::from("/workspace/new.rs"),
            ],
            FsEventKind::Renamed,
            &root,
        );
        match delta {
            FileIndexDelta::Batch(deltas) => {
                assert_eq!(deltas.len(), 2);
                match &deltas[0] {
                    FileIndexDelta::Remove(p) => assert_eq!(p[0], "old.rs"),
                    _ => panic!("Expected Remove"),
                }
                match &deltas[1] {
                    FileIndexDelta::Add(e) => assert_eq!(e[0].0, "new.rs"),
                    _ => panic!("Expected Add"),
                }
            }
            _ => panic!("Expected Batch delta"),
        }
    }

    #[test]
    fn fs_event_to_delta_modify_is_empty() {
        let root = PathBuf::from("/workspace");
        let delta = fs_event_to_delta(
            &[PathBuf::from("/workspace/file.rs")],
            FsEventKind::Modified,
            &root,
        );
        assert!(delta.is_empty());
    }

    #[test]
    fn is_under_hidden_dir_positive() {
        assert!(is_under_hidden_dir(
            &PathBuf::from("/workspace/.claude/worktrees/abc/src/main.rs"),
            &PathBuf::from("/workspace"),
        ));
    }

    #[test]
    fn is_under_hidden_dir_ignores_cwd_components() {
        assert!(!is_under_hidden_dir(
            &PathBuf::from("/home/user/.config/project/src/lib.rs"),
            &PathBuf::from("/home/user/.config/project"),
        ));
    }

    #[test]
    fn is_under_hidden_dir_unrelated_path_is_not_dropped() {
        // strip_prefix fails and the (nonexistent) paths can't canonicalize, so
        // we must NOT scan the absolute path and treat the `.cache` ancestor as
        // hidden — that would silently drop every event for a repo under one.
        assert!(!is_under_hidden_dir(
            &PathBuf::from("/home/user/.cache/repo/src/main.rs"),
            &PathBuf::from("/some/other/root"),
        ));
    }

    #[test]
    fn parse_diff_name_status() {
        use echo_agent_codebase_graph::FileEventKind;
        let root = Path::new("/repo");

        let ev = parse_diff_name_status_line("M\tsrc/main.rs", root).unwrap();
        assert_eq!(ev.kind, FileEventKind::Modified);

        let ev = parse_diff_name_status_line("A\tnew_file.rs", root).unwrap();
        assert_eq!(ev.kind, FileEventKind::Created);

        let ev = parse_diff_name_status_line("D\told_file.rs", root).unwrap();
        assert_eq!(ev.kind, FileEventKind::Removed);

        let ev = parse_diff_name_status_line("R100\told.rs\tnew.rs", root).unwrap();
        assert_eq!(ev.kind, FileEventKind::Renamed);

        assert!(parse_diff_name_status_line("", root).is_none());
    }

    #[test]
    fn needs_watcher_truth_table() {
        assert!(!FsWatchCapabilities::none().needs_watcher());
        assert!(!FsWatchCapabilities::default().needs_watcher());
        for field_setter in [
            |c: &mut FsWatchCapabilities| c.client_notify = true,
            |c: &mut FsWatchCapabilities| c.hunk_tracking = true,
            |c: &mut FsWatchCapabilities| c.code_nav = true,
            |c: &mut FsWatchCapabilities| c.git_head = true,
        ] {
            let mut caps = FsWatchCapabilities::default();
            field_setter(&mut caps);
            assert!(caps.needs_watcher());
        }
    }

    fn inputs(on: bool, git_head_changed: Option<bool>) -> CapabilityInputs {
        CapabilityInputs {
            client_notify: on,
            hunk_tracking: on,
            code_nav: on,
            git_head_changed,
        }
    }

    #[test]
    fn resolve_reflects_capabilities() {
        // git_head is opt-in: absent => off, advertised value otherwise.
        assert!(!FsWatchCapabilities::resolve(inputs(false, None)).git_head);
        assert!(FsWatchCapabilities::resolve(inputs(false, Some(true))).git_head);
        assert!(!FsWatchCapabilities::resolve(inputs(false, Some(false))).git_head);
        // Distinct per-field values catch a field-mapping transposition in resolve.
        let caps = FsWatchCapabilities::resolve(CapabilityInputs {
            client_notify: true,
            hunk_tracking: false,
            code_nav: true,
            git_head_changed: Some(false),
        });
        assert!(caps.client_notify && !caps.hunk_tracking && caps.code_nav && !caps.git_head);
    }

    #[test]
    fn git_head_dedup_key_identity() {
        let base = git_head_dedup_key(Some("main"), false, Some("/repo"), Some("abc123"));
        // Every dimension is part of the identity.
        assert_ne!(
            base,
            git_head_dedup_key(Some("dev"), false, Some("/repo"), Some("abc123"))
        );
        assert_ne!(
            base,
            git_head_dedup_key(Some("main"), true, Some("/repo"), Some("abc123"))
        );
        assert_ne!(
            base,
            git_head_dedup_key(Some("main"), false, Some("/other"), Some("abc123"))
        );
        // A same-branch commit moves HEAD and must not dedup away — the
        // changes panel relies on this to drop the now-committed files.
        assert_ne!(
            base,
            git_head_dedup_key(Some("main"), false, Some("/repo"), Some("def456"))
        );
        // Detached HEAD (None branch) is stable and distinct from a real branch.
        assert_eq!(
            git_head_dedup_key(None, false, None, None),
            git_head_dedup_key(None, false, None, None)
        );
        assert_ne!(
            base,
            git_head_dedup_key(None, false, Some("/repo"), Some("abc123"))
        );
        // Swapping branch and main_repo must not collide (the NUL separator).
        assert_ne!(
            git_head_dedup_key(Some("a"), false, Some("b"), None),
            git_head_dedup_key(Some("b"), false, Some("a"), None),
        );
    }

    #[test]
    fn hunk_not_built_when_actor_disabled() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let deps = FsWatchDeps {
            gateway: crate::test_support::lsp_runtime::test_gateway(),
            session_id: "s".into(),
            cwd: PathBuf::from("/repo"),
            index_root: PathBuf::from("/repo"),
            hunk_tracker: HunkTrackerHandle::noop(),
            hunk_tracking_enabled: false,
            codebase_indexes: Arc::new(parking_lot::Mutex::new(CodebaseIndexManager::new())),
            client_fs_config: None,
            persistence_tx: tx,
            last_reported_branch: Arc::new(parking_lot::Mutex::new(None)),
            status_wake: Arc::default(),
        };
        let plan = FsWatchPlan::build(
            FsWatchCapabilities {
                hunk_tracking: true,
                ..Default::default()
            },
            deps,
        );
        assert!(plan.hunk.is_none());
    }

    fn batch() -> FsBatch {
        (
            vec![PathBuf::from("/repo/src/lib.rs")],
            FsEventKind::Modified,
        )
    }

    /// Head moved: the buffered flood is stale, so it is discarded (no replay)
    /// and the rebuild is requested; the buffer ends empty.
    #[test]
    fn head_change_discards_buffer_and_requests_rebuild() {
        let mut op_buffer = vec![batch(), batch()];
        let (replay, rebuild) = resolve_completed_op(true, &mut op_buffer);
        assert!(replay.is_empty());
        assert!(rebuild);
        assert!(op_buffer.is_empty());
    }

    #[test]
    fn head_change_empty_buffer_requests_rebuild() {
        let mut op_buffer: Vec<FsBatch> = Vec::new();
        let (replay, rebuild) = resolve_completed_op(true, &mut op_buffer);
        assert!(replay.is_empty());
        assert!(rebuild);
    }

    /// EdenFS-degraded `goto`: head reads unchanged but a flood was buffered, so
    /// the batches are replayed AND the rebuild is requested. Guards the ordering:
    /// sampling occupancy after the drain would read empty and drop the rebuild.
    #[test]
    fn degraded_goto_replays_buffer_and_requests_rebuild() {
        let mut op_buffer = vec![batch(), batch()];
        let (replay, rebuild) = resolve_completed_op(false, &mut op_buffer);
        assert_eq!(replay.len(), 2);
        assert!(rebuild);
        assert!(op_buffer.is_empty());
    }

    #[test]
    fn no_op_completion_skips_rebuild() {
        let mut op_buffer: Vec<FsBatch> = Vec::new();
        let (replay, rebuild) = resolve_completed_op(false, &mut op_buffer);
        assert!(replay.is_empty());
        assert!(!rebuild);
    }

    fn files_changed(path: &str) -> FsEvent {
        FsEvent::FilesChanged {
            paths: vec![PathBuf::from(path)],
            kind: FsEventKind::Modified,
        }
    }

    #[test]
    fn on_event_buffers_during_op_and_forwards_when_live() {
        let mut in_op = false;
        let mut buf = Vec::new();
        assert!(matches!(
            on_event(FsEvent::GitOperationStarted, &mut in_op, &mut buf),
            Outcome::Buffered
        ));
        assert!(in_op);
        // In-op changes accumulate; live changes forward.
        assert!(matches!(
            on_event(files_changed("/r/a"), &mut in_op, &mut buf),
            Outcome::Buffered
        ));
        assert_eq!(buf.len(), 1);
        in_op = false;
        assert!(matches!(
            on_event(files_changed("/r/b"), &mut in_op, &mut buf),
            Outcome::Forward(_)
        ));
        assert_eq!(buf.len(), 1, "live change is not buffered");
    }

    #[test]
    fn on_event_completed_drains_and_clears_in_op() {
        let mut in_op = true;
        let mut buf = vec![batch(), batch()];
        match on_event(
            FsEvent::GitOperationCompleted {
                head_changed: false,
            },
            &mut in_op,
            &mut buf,
        ) {
            Outcome::Completed { replay, rebuild } => {
                assert_eq!(replay.len(), 2);
                assert!(rebuild);
            }
            _ => panic!("expected Completed"),
        }
        assert!(!in_op);
        assert!(buf.is_empty());
    }

    #[test]
    fn on_event_op_buffer_cap_recovers() {
        // A lost completion boundary would otherwise grow the buffer forever and
        // swallow every event; the cap recovers (unstick, clear, force rebuild).
        let mut in_op = true;
        let mut buf: Vec<FsBatch> = (0..MAX_OP_BUFFER).map(|_| batch()).collect();
        match on_event(files_changed("/r/a"), &mut in_op, &mut buf) {
            Outcome::Refresh { rebuild } => assert!(rebuild),
            _ => panic!("expected Refresh recovery"),
        }
        assert!(!in_op);
        assert!(buf.is_empty());
    }

    #[test]
    fn on_resync_unsticks_and_refreshes() {
        let mut in_op = true;
        let mut buf = vec![batch()];
        match on_resync(&mut in_op, &mut buf) {
            Outcome::Refresh { rebuild } => assert!(rebuild),
            _ => panic!("expected Refresh"),
        }
        assert!(!in_op);
        assert!(buf.is_empty());
    }

    #[test]
    fn next_deadline_picks_earlier_of_quiet_and_max() {
        let mut d = Debounce::idle();
        assert_eq!(d.next_deadline(), None);

        d.arm();
        // Fresh arm: the quiet window precedes the max-wait cap.
        assert!(d.quiet_deadline < d.max_deadline);
        assert_eq!(d.next_deadline(), d.quiet_deadline);

        // Sustained burst pushes quiet past the cap; the cap forces the refresh.
        d.quiet_deadline = Some(d.max_deadline.unwrap() + REFRESH_DEBOUNCE_QUIET);
        assert_eq!(d.next_deadline(), d.max_deadline);

        d.clear();
        assert!(!d.active());
    }

    #[tokio::test(start_paused = true)]
    async fn debounce_bump_arms_then_extends_quiet_keeping_cap() {
        let mut d = Debounce::idle();
        d.bump(); // idle -> arm
        assert!(d.active());
        let (quiet0, max0) = (d.quiet_deadline.unwrap(), d.max_deadline.unwrap());

        tokio::time::advance(Duration::from_millis(100)).await;
        d.bump(); // active -> extend the quiet window, keep the max-wait cap
        assert!(d.quiet_deadline.unwrap() > quiet0);
        assert_eq!(d.max_deadline.unwrap(), max0, "bump must not push the cap");
    }

    /// Advance the paused clock to `target`, resolving every debounce
    /// deadline that falls due on the way through the real settle arm
    /// (`on_settle_due`) under the given `in_op`; count the fires. Deadlines
    /// coinciding with `target` resolve before the event at `target` is
    /// processed, matching the loop's biased select order.
    async fn advance_resolving_settles(
        target: tokio::time::Instant,
        debounce: &mut Debounce,
        in_op: bool,
        rebuild_flag: &mut bool,
        in_op_defers: &mut u32,
        fires: &mut usize,
    ) {
        loop {
            let now = tokio::time::Instant::now();
            match debounce.next_deadline().filter(|d| *d <= target) {
                Some(d) => {
                    if d > now {
                        tokio::time::advance(d - now).await;
                    }
                    if let SettleAction::Fire { .. } =
                        on_settle_due(in_op, false, debounce, rebuild_flag, in_op_defers)
                    {
                        *fires += 1;
                    }
                }
                None => {
                    if target > now {
                        tokio::time::advance(target - now).await;
                    }
                    return;
                }
            }
        }
    }

    /// Feed the event stream a K-pick rebase produces through the real
    /// `on_event`/`note_refresh_request`/`on_settle_due` seams; return the
    /// number of refresh fires. Models fsnotify's settle semantics: the
    /// source holds Completed for `SETTLE_MS` after each unlock, so picks
    /// whose lock-free gap fits inside the settle window merge into a single
    /// Started/Completed pair, while slower cadences emit per-pick pairs
    /// whose Completed lags the unlock by the settle window.
    async fn run_rebase_cadence(
        picks: usize,
        pick_period: Duration,
        pick_duration: Duration,
    ) -> usize {
        let settle = Duration::from_millis(echo_agent_fsnotify::SETTLE_MS);
        // Uniform cadence: either every re-lock lands inside the previous
        // pick's settle window (one merged op) or none does (per-pick pairs).
        let merged = pick_period - pick_duration <= settle;

        let mut in_op = false;
        let mut op_buffer: Vec<FsBatch> = Vec::new();
        let mut debounce = Debounce::idle();
        let mut rebuild_flag = false;
        let mut in_op_defers = 0u32;
        let mut fires = 0usize;
        let start = tokio::time::Instant::now();

        for pick in 0..picks {
            let t_started = start + pick_period * pick as u32;
            let t_completed = t_started + pick_duration + settle;

            if pick == 0 || !merged {
                advance_resolving_settles(
                    t_started,
                    &mut debounce,
                    in_op,
                    &mut rebuild_flag,
                    &mut in_op_defers,
                    &mut fires,
                )
                .await;
                assert!(matches!(
                    on_event(FsEvent::GitOperationStarted, &mut in_op, &mut op_buffer),
                    Outcome::Buffered
                ));
            }

            if pick == picks - 1 || !merged {
                advance_resolving_settles(
                    t_completed,
                    &mut debounce,
                    in_op,
                    &mut rebuild_flag,
                    &mut in_op_defers,
                    &mut fires,
                )
                .await;
                match on_event(
                    FsEvent::GitOperationCompleted { head_changed: true },
                    &mut in_op,
                    &mut op_buffer,
                ) {
                    Outcome::Completed { rebuild, .. } => {
                        note_refresh_request(rebuild, &mut debounce, &mut rebuild_flag);
                    }
                    other => panic!(
                        "expected Completed, got {:?}",
                        std::mem::discriminant(&other)
                    ),
                }
            }
        }

        // Drain the trailing window after the last pick.
        let idle_tail = REFRESH_DEBOUNCE_MAX_WAIT + REFRESH_DEBOUNCE_QUIET;
        advance_resolving_settles(
            start + pick_period * picks as u32 + idle_tail,
            &mut debounce,
            in_op,
            &mut rebuild_flag,
            &mut in_op_defers,
            &mut fires,
        )
        .await;
        assert!(!debounce.active(), "debounce must be drained at the end");
        fires
    }

    /// Picks slower than the settle window (1s apart, the big-repo rebase
    /// cadence): the source still emits per-pick pairs, but every quiet
    /// deadline lands inside the next pick's op window, so the settle arm
    /// defers it — the single refresh fires after the last pick completes.
    /// Before the in-op deferral this fired once per inter-pick gap.
    #[tokio::test(start_paused = true)]
    async fn rebase_cadence_defers_mid_op_and_fires_once() {
        let fires =
            run_rebase_cadence(6, Duration::from_millis(1000), Duration::from_millis(100)).await;
        assert_eq!(fires, 1, "one refresh per rebase, after the last pick");
    }

    /// Picks faster than the settle window: fsnotify merges all 16 lock
    /// cycles into one operation, so the loop sees a single pair and fires a
    /// single refresh. Before the merge this hit the max-wait cap twice.
    #[tokio::test(start_paused = true)]
    async fn dense_rebase_cadence_merges_into_one_fire() {
        let fires =
            run_rebase_cadence(16, Duration::from_millis(400), Duration::from_millis(200)).await;
        assert_eq!(fires, 1, "one refresh for the merged operation");
    }

    /// on_settle_due decision table: mid-op and mid-refresh defer (re-armed,
    /// rebuild request kept); otherwise fire, taking the request.
    #[tokio::test(start_paused = true)]
    async fn settle_due_defers_in_op_or_refreshing_and_fires_otherwise() {
        let mut d = Debounce::idle();
        let mut rebuild = true;
        let mut defers = 0u32;

        d.arm();
        assert!(matches!(
            on_settle_due(true, false, &mut d, &mut rebuild, &mut defers),
            SettleAction::Defer
        ));
        assert!(d.active(), "in-op deferral must re-arm");
        assert!(rebuild, "in-op deferral must keep the rebuild request");
        assert_eq!(defers, 1, "in-op deferral counts toward the give-up bound");

        assert!(matches!(
            on_settle_due(false, true, &mut d, &mut rebuild, &mut defers),
            SettleAction::Defer
        ));
        assert!(d.active(), "single-flight deferral must re-arm");
        assert_eq!(defers, 1, "single-flight deferral must not count");

        match on_settle_due(false, false, &mut d, &mut rebuild, &mut defers) {
            SettleAction::Fire { rebuild } => assert!(rebuild, "fire carries the request"),
            SettleAction::Defer => panic!("idle settle must fire"),
        }
        assert!(!d.active(), "fire leaves the debounce cleared");
        assert!(!rebuild, "fire takes the rebuild request");
        assert_eq!(defers, 0, "fire resets the give-up counter");
    }

    /// The give-up bound: a wedged op (stale `.git/index.lock`, no Completed
    /// ever) defers exactly [`MAX_CONSECUTIVE_IN_OP_DEFERS`] times, then the
    /// refresh fires anyway and the cycle restarts — refreshes are starved
    /// for at most ~a minute, not forever.
    #[tokio::test(start_paused = true)]
    async fn wedged_op_deferral_is_bounded() {
        let mut d = Debounce::idle();
        let mut rebuild = true;
        let mut defers = 0u32;

        d.arm();
        for i in 0..MAX_CONSECUTIVE_IN_OP_DEFERS {
            assert!(
                matches!(
                    on_settle_due(true, false, &mut d, &mut rebuild, &mut defers),
                    SettleAction::Defer
                ),
                "defer {i} must still hold back"
            );
        }
        match on_settle_due(true, false, &mut d, &mut rebuild, &mut defers) {
            SettleAction::Fire { rebuild } => {
                assert!(rebuild, "the forced fire carries the pending request");
            }
            SettleAction::Defer => panic!("the give-up bound must force a fire"),
        }
        assert_eq!(defers, 0, "forced fire restarts the bound");
    }

    /// The max-wait cap still owns non-op bursts: sustained sub-quiet bumps
    /// (e.g. background-fetch ref churn arriving as GitMetaChanged) keep
    /// pushing the quiet window, so the 3s cap is what forces the refresh.
    #[tokio::test(start_paused = true)]
    async fn sustained_non_op_bumps_fire_on_max_wait_cap() {
        let mut debounce = Debounce::idle();
        let mut rebuild_flag = false;
        let mut in_op_defers = 0u32;
        let mut fires = 0usize;
        let start = tokio::time::Instant::now();

        // Bumps every 400ms (< 500ms quiet) from t=0 to t=2.8s: quiet never
        // elapses between bumps, so only the cap (armed at t=0, due t=3.0s,
        // before the last quiet deadline at 3.3s) can fire.
        for k in 0..8u32 {
            advance_resolving_settles(
                start + Duration::from_millis(400) * k,
                &mut debounce,
                false,
                &mut rebuild_flag,
                &mut in_op_defers,
                &mut fires,
            )
            .await;
            note_refresh_request(true, &mut debounce, &mut rebuild_flag);
        }
        assert_eq!(fires, 0, "no quiet window elapsed during the burst");

        advance_resolving_settles(
            start + Duration::from_millis(3050),
            &mut debounce,
            false,
            &mut rebuild_flag,
            &mut in_op_defers,
            &mut fires,
        )
        .await;
        assert_eq!(fires, 1, "the max-wait cap must force the fire at 3s");
        assert!(!debounce.active(), "cap fire clears the debounce");
    }

    /// The residual mid-op hole: a debounce armed by pre-op activity (a
    /// git-meta refresh request here) must not fire while the op is in
    /// flight; the deferral re-arms it and the refresh runs exactly once
    /// after Completed, rebuild request intact.
    #[tokio::test(start_paused = true)]
    async fn debounce_armed_pre_op_defers_mid_op_and_fires_once_after_completed() {
        let mut in_op = false;
        let mut op_buffer: Vec<FsBatch> = Vec::new();
        let mut debounce = Debounce::idle();
        let mut rebuild_flag = false;
        let mut defers = 0u32;

        match on_event(
            FsEvent::GitMetaChanged {
                kind: echo_agent_fsnotify::GitMetaKind::RefsChanged,
            },
            &mut in_op,
            &mut op_buffer,
        ) {
            Outcome::Refresh { rebuild } => {
                note_refresh_request(rebuild, &mut debounce, &mut rebuild_flag);
            }
            other => panic!("expected Refresh, got {:?}", std::mem::discriminant(&other)),
        }

        // The op starts before the quiet window elapses.
        tokio::time::advance(Duration::from_millis(100)).await;
        assert!(matches!(
            on_event(FsEvent::GitOperationStarted, &mut in_op, &mut op_buffer),
            Outcome::Buffered
        ));

        // The pre-op deadline falls due mid-op: deferred, request kept.
        let due = debounce.next_deadline().expect("debounce armed pre-op");
        tokio::time::advance(due - tokio::time::Instant::now()).await;
        assert!(matches!(
            on_settle_due(in_op, false, &mut debounce, &mut rebuild_flag, &mut defers),
            SettleAction::Defer
        ));
        assert!(debounce.active() && rebuild_flag);

        // Op completes (no head move, nothing buffered): the re-armed
        // deadline fires exactly once, with the preserved rebuild request.
        match on_event(
            FsEvent::GitOperationCompleted {
                head_changed: false,
            },
            &mut in_op,
            &mut op_buffer,
        ) {
            Outcome::Completed { rebuild, .. } => {
                note_refresh_request(rebuild, &mut debounce, &mut rebuild_flag);
            }
            other => panic!(
                "expected Completed, got {:?}",
                std::mem::discriminant(&other)
            ),
        }
        let due = debounce.next_deadline().expect("bumped by Completed");
        tokio::time::advance(due - tokio::time::Instant::now()).await;
        match on_settle_due(in_op, false, &mut debounce, &mut rebuild_flag, &mut defers) {
            SettleAction::Fire { rebuild } => {
                assert!(rebuild, "the pre-op rebuild request survives the deferral");
            }
            SettleAction::Defer => panic!("op is over; the refresh must fire"),
        }
        assert!(!debounce.active(), "single fire, nothing left armed");
    }
}
