pub(crate) mod checkpoint;
pub(crate) mod checkpoint_store;
pub mod file_state;
pub mod git;
pub(crate) mod git_gate;
pub mod jj;
pub(crate) mod swap_policy;
pub mod tool_config;
use crate::capability::CapabilityMode;
use crate::config::{MemoryConfig, SessionContextFactory};
use crate::file_system::{AsyncFsWrapper, LocalFs};
use crate::hub::{HubConfig, HubHandle};
use crate::session::file_state::FileStateTracker;
use echo_agent_computer_hub_mcp_adapter::McpBridgeHandle;
use echo_agent_hunk_tracker::HunkTrackerHandle;
use echo_agent_mcp::servers::McpState;
use echo_agent_tool_protocol::ToolId;
use echo_agent_tool_runtime::WorkspaceViewerContext;
use echo_agent_tools::notification::AcknowledgedToolNotification;
use echo_agent_tools::notification::types::ToolNotificationHandle;
use echo_agent_tools::registry::types::{FinalizedToolset, ToolConfig, ToolServerConfig};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
/// Minimal result types for git error reporting (duplicated from shell session/result).
pub mod result {
    use serde::Serialize;
    #[derive(Debug, Serialize)]
    pub struct ExtMethodError {
        pub code: i32,
        pub message: String,
        pub data: Option<serde_json::Value>,
    }
    impl ExtMethodError {
        pub fn with_data(code: i32, msg: String, data: impl Serialize) -> Self {
            Self {
                code,
                message: msg,
                data: serde_json::to_value(data).ok(),
            }
        }
    }
    #[derive(Debug)]
    pub struct ExtMethodResult<T> {
        pub result: Option<T>,
        pub error: Option<serde_json::Value>,
    }
}
/// Per-session state held in [`WorkspaceShared::sessions`].
///
/// The `effective_tool_config` baseline and the resolved `toolset` are
/// kept under a single `RwLock` so a hot reload swaps both atomically.
pub struct WorkspaceSession {
    pub(crate) session_id: String,
    pub(crate) cwd: PathBuf,
    pub(crate) session_env: Arc<HashMap<String, String>>,
    pub(crate) capability_mode: CapabilityMode,
    pub(crate) depth: u32,
    pub(crate) fork_budget: u32,
    pub(crate) hunk_tracker: HunkTrackerHandle,
    /// Cancel token for the workspace-spawned [`HunkTrackerActor`] backing
    /// [`Self::hunk_tracker`], fired on session teardown by
    /// [`Self::cancel_hunk_tracker`]. `None` when the tracker is externally
    /// owned (e.g. `create_session_with_tracker` / local shell mode).
    ///
    /// [`HunkTrackerActor`]: echo_agent_hunk_tracker::HunkTrackerActor
    pub(crate) hunk_tracker_cancel: Option<tokio_util::sync::CancellationToken>,
    pub(crate) file_state_tracker: Arc<FileStateTracker>,
    /// Per-turn hunk deltas keyed by `prompt_index`, captured at finalize and
    /// replayed on rewind (only when `workspace_rewind_hunks` is on). The live
    /// restore source; the durable on-disk mirror is the [`checkpoint_store`] field.
    ///
    /// [`checkpoint_store`]: WorkspaceSession::checkpoint_store
    pub(crate) hunk_checkpoints:
        Arc<tokio::sync::Mutex<HashMap<usize, echo_agent_hunk_tracker::HunkTurnDelta>>>,
    /// Git domain of the per-prompt rewind checkpoints (HEAD + staged set).
    pub(crate) git_checkpoints: crate::session::git::GitCheckpointStore,
    /// Disk-backed durability mirror for finalized checkpoints, fronted by an
    /// in-memory cache. Gated by `workspace_rewind_durable` (off = no disk I/O,
    /// legacy path). Co-located in the working tree so the rootfs snapshot carries
    /// it and a restored session rehydrates the cache. Mirror only — restore stays in-process.
    pub(crate) checkpoint_store: crate::session::checkpoint_store::CheckpointStore,
    pub(crate) async_fs: AsyncFsWrapper,
    inner: RwLock<WorkspaceSessionInner>,
    /// Per-session lock that serialises `update_tool_config` calls.
    pub(crate) update_lock: tokio::sync::Mutex<()>,
    /// Per-session MCP state (owned clients, etc.).
    pub(crate) mcp_state: Arc<tokio::sync::Mutex<McpState>>,
    /// MCP bridges kept alive for the session lifetime.
    pub(crate) mcp_bridges: tokio::sync::Mutex<Vec<McpBridgeHandle>>,
    /// Qualified tool IDs registered on the server for this session's MCP tools.
    pub(crate) mcp_tool_ids: tokio::sync::Mutex<Vec<ToolId>>,
    /// Per-user feature-flag bag resolved at session-bind time, frozen for
    /// the session lifetime. `None` → tools use their safe defaults.
    pub(crate) viewer_ctx: Option<WorkspaceViewerContext>,
    /// Auto-approve (YOLO) state. Seeded from `session.bind` metadata,
    /// refreshed by each before-turn hook.
    pub(crate) yolo_mode: std::sync::atomic::AtomicBool,
    /// Session-lifetime terminal backend (background-task registry +
    /// persistent shell). Created once at session construction; every toolset
    /// re-resolve reuses it, so background tasks and shell state survive
    /// toolset swaps. Its child processes die only via `kill_task`,
    /// [`Self::shutdown_terminal_backend`] (`drop_session`/evict), or process
    /// exit.
    ///
    /// Local-mode exception: `bind_local_session` installs an externally
    /// built toolset via plain [`Self::replace`], so that toolset's
    /// `Terminal` resource is the shell's own backend while this one sits
    /// idle as the sole safe teardown target. Never adopt an externally
    /// owned backend into this field (drop/evict would SIGKILL a backend
    /// shared with the shell) and never query this field for the live task
    /// table — the toolset's `Terminal` resource is the source of truth.
    terminal_backend: crate::config::SessionTerminalBackend,
    /// Canonical JSON of the explicit `session.bind` toolset this session was
    /// created (or last rebound) with. `None` when the session was resolved
    /// from the workspace default (no explicit toolset in the bind metadata).
    /// Lets a rebind detect a config change and re-resolve instead of silently
    /// reusing a stale toolset (e.g. a session created by a metadata-less
    /// hub revive bind that a config-carrying client rebind must correct).
    bind_tool_config_fingerprint: std::sync::Mutex<Option<serde_json::Value>>,
    /// The last snapshot-driven rebuild failed and kept a stale toolset;
    /// cleared by any successful install. While set, an identical-config
    /// re-apply (update RPC or owner rebind) heals instead of reusing.
    stale_resolve: std::sync::atomic::AtomicBool,
    /// Whether this session forwards `BackgroundTaskCompleted` system notifications.
    #[allow(dead_code)]
    system_notifications: bool,
    /// Per-session notification sender, re-applied across toolset re-resolves.
    system_notify_handle: Option<ToolNotificationHandle>,
    /// Receiver paired with `system_notify_handle`, taken once by the forwarder.
    #[allow(dead_code)]
    pending_notif_rx: tokio::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<AcknowledgedToolNotification>>,
    >,
    /// Spawned system-notify producers (forwarder, preview-state watcher).
    /// Sync mutex so the sync teardown path can abort without an await.
    system_notify_producers: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
    /// Guest↔model path mapping. Set once at bind from `session_root`.
    path_virtualization: OnceLock<crate::path_virtualization::PathVirtualization>,
    /// Rewritten bind cwd installed on rebind when path virt turns on after
    /// the session was first created without `session_root`.
    cwd_override: OnceLock<PathBuf>,
}
struct WorkspaceSessionInner {
    effective_tool_config: Arc<ToolServerConfig>,
    toolset: Arc<FinalizedToolset>,
}
impl std::fmt::Debug for WorkspaceSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceSession")
            .field("session_id", &self.session_id)
            .field("cwd", &self.cwd())
            .field("capability_mode", &self.capability_mode)
            .field("depth", &self.depth)
            .field("fork_budget", &self.fork_budget)
            .finish_non_exhaustive()
    }
}
/// Copy pre-virt working-tree files into `new` so remounted hunk accept
/// still sees content written under the old cwd. Skips the dest subtree
/// and sibling session trees (`/workspace/<other-uuid>`).
fn relocate_pre_virt_files(old: &Path, new: &Path) {
    if std::fs::create_dir_all(new).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(old) else {
        return;
    };
    for entry in entries.flatten() {
        let src = entry.path();
        if src == new {
            continue;
        }
        let Ok(meta) = std::fs::symlink_metadata(&src) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        if meta.is_dir() && uuid::Uuid::parse_str(&entry.file_name().to_string_lossy()).is_ok() {
            continue;
        }
        let dest = new.join(entry.file_name());
        if dest.symlink_metadata().is_ok() {
            continue;
        }
        if meta.is_dir() {
            let _ = copy_dir_all(&src, &dest);
        } else {
            let _ = std::fs::copy(&src, &dest);
        }
    }
}
fn copy_dir_all(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let to = dest.join(entry.file_name());
        if meta.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
impl WorkspaceSession {
    pub(crate) fn new(
        session_id: String,
        cwd: PathBuf,
        session_env: Arc<HashMap<String, String>>,
        capability_mode: CapabilityMode,
        depth: u32,
        fork_budget: u32,
        effective_tool_config: Arc<ToolServerConfig>,
        toolset: Arc<FinalizedToolset>,
        terminal_backend: crate::config::SessionTerminalBackend,
        hunk_tracker: HunkTrackerHandle,
        hunk_tracker_cancel: Option<tokio_util::sync::CancellationToken>,
        viewer_ctx: Option<WorkspaceViewerContext>,
        #[allow(dead_code)] system_notifications: bool,
        system_notify_channel: Option<(
            ToolNotificationHandle,
            tokio::sync::mpsc::UnboundedReceiver<AcknowledgedToolNotification>,
        )>,
    ) -> Self {
        let (system_notify_handle, pending_notif_rx) = match system_notify_channel {
            Some((handle, rx)) => (Some(handle), Some(rx)),
            None => (None, None),
        };
        let async_fs = AsyncFsWrapper::new(Arc::new(LocalFs::new(cwd.clone())));
        let file_state_tracker = Arc::new(FileStateTracker::new());
        let checkpoint_store =
            crate::session::checkpoint_store::CheckpointStore::new(&cwd, &session_id);
        Self {
            session_id,
            cwd,
            session_env,
            capability_mode,
            depth,
            fork_budget,
            hunk_tracker,
            hunk_tracker_cancel,
            file_state_tracker,
            hunk_checkpoints: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            git_checkpoints: crate::session::git::GitCheckpointStore::new(),
            checkpoint_store,
            async_fs,
            inner: RwLock::new(WorkspaceSessionInner {
                effective_tool_config,
                toolset,
            }),
            terminal_backend,
            update_lock: tokio::sync::Mutex::new(()),
            bind_tool_config_fingerprint: std::sync::Mutex::new(None),
            stale_resolve: std::sync::atomic::AtomicBool::new(false),
            mcp_state: Arc::new(tokio::sync::Mutex::new(McpState::new(vec![]))),
            mcp_bridges: tokio::sync::Mutex::new(Vec::new()),
            mcp_tool_ids: tokio::sync::Mutex::new(Vec::new()),
            viewer_ctx,
            yolo_mode: std::sync::atomic::AtomicBool::new(false),
            system_notifications,
            system_notify_handle,
            #[allow(dead_code)]
            pending_notif_rx: tokio::sync::Mutex::new(pending_notif_rx),
            system_notify_producers: std::sync::Mutex::new(Vec::new()),
            path_virtualization: OnceLock::new(),
            cwd_override: OnceLock::new(),
        }
    }
    pub(crate) fn set_path_virtualization(
        &self,
        mapping: crate::path_virtualization::PathVirtualization,
    ) {
        let _ = self.path_virtualization.set(mapping);
    }
    /// Install a rewritten bind cwd on a session that already exists.
    ///
    /// First-bind `create_session` already constructed `cwd`; rebind only
    /// fills `OnceLock` path virt and would otherwise leave a stale
    /// `/workspace` (or `/workspace/artifacts`) cwd against inbound
    /// `/workspace/<conv>` rewrites. Remounts LocalFs, the checkpoint
    /// store, the hunk tracker, and the live toolset `Cwd` so relative
    /// work follows the real session tree.
    pub(crate) async fn set_cwd_for_virtualization(&self, cwd: PathBuf) -> Result<(), String> {
        if self.cwd() == cwd.as_path() {
            return Ok(());
        }
        let old = self.cwd().to_path_buf();
        if old != cwd {
            let dest = cwd.clone();
            let _ = tokio::task::spawn_blocking(move || relocate_pre_virt_files(&old, &dest)).await;
        }
        if !self.checkpoint_store.remount(&cwd, &self.session_id).await {
            return Err("checkpoint remount failed".into());
        }
        let _ = self.cwd_override.set(cwd.clone());
        self.async_fs.remount_root(cwd.clone());
        self.hunk_tracker.set_working_dir(cwd.clone()).await;
        let toolset = self.toolset();
        let mut res = toolset.resources.lock().await;
        res.insert(echo_agent_tools::types::resources::Cwd(cwd));
        Ok(())
    }
    pub(crate) fn path_virtualization(
        &self,
    ) -> Option<&crate::path_virtualization::PathVirtualization> {
        self.path_virtualization.get()
    }
    /// Whether this session opted into `BackgroundTaskCompleted` system
    /// notifications.
    #[allow(dead_code)]
    pub(crate) fn system_notifications(&self) -> bool {
        self.system_notifications
    }
    /// The per-session notification sender, re-applied on every toolset
    /// re-resolve so notifications keep flowing to the forwarder's channel.
    pub(crate) fn system_notify_handle(&self) -> Option<ToolNotificationHandle> {
        self.system_notify_handle.clone()
    }
    /// Hand the notification receiver to the forwarder. Works once.
    #[allow(dead_code)]
    pub(crate) async fn take_pending_notif_rx(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<AcknowledgedToolNotification>> {
        self.pending_notif_rx.lock().await.take()
    }
    /// True once a producer set has been tracked; finalize spawns at most one
    /// (a re-finalize must not abort a forwarder it cannot respawn).
    #[allow(dead_code)]
    pub(crate) fn has_system_notify_producers(&self) -> bool {
        !self
            .system_notify_producers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }
    /// Track the spawned system-notify producers, aborting any previous set.
    #[allow(dead_code)]
    pub(crate) fn set_system_notify_producers(&self, handles: Vec<tokio::task::JoinHandle<()>>) {
        let mut guard = self
            .system_notify_producers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for old in guard.drain(..) {
            old.abort();
        }
        *guard = handles;
    }
    /// Abort every tracked system-notify producer on teardown.
    pub(crate) fn abort_system_notify_producers(&self) {
        for handle in self
            .system_notify_producers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            handle.abort();
        }
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    pub fn cwd(&self) -> &Path {
        self.cwd_override.get().map_or(&self.cwd, PathBuf::as_path)
    }
    pub fn session_env(&self) -> &Arc<HashMap<String, String>> {
        &self.session_env
    }
    pub fn capability_mode(&self) -> CapabilityMode {
        self.capability_mode
    }
    pub fn depth(&self) -> u32 {
        self.depth
    }
    pub fn fork_budget(&self) -> u32 {
        self.fork_budget
    }
    pub fn hunk_tracker(&self) -> &HunkTrackerHandle {
        &self.hunk_tracker
    }
    /// Per-user feature-flag bag resolved at session-bind time.
    pub fn viewer_ctx(&self) -> Option<&WorkspaceViewerContext> {
        self.viewer_ctx.as_ref()
    }
    pub fn yolo_mode(&self) -> bool {
        self.yolo_mode.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn set_yolo_mode(&self, enabled: bool) {
        self.yolo_mode
            .store(enabled, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn file_state_tracker(&self) -> &Arc<FileStateTracker> {
        &self.file_state_tracker
    }
    /// Git domain of the per-prompt rewind checkpoints.
    pub fn git_checkpoints(&self) -> &crate::session::git::GitCheckpointStore {
        &self.git_checkpoints
    }
    pub fn async_fs(&self) -> &AsyncFsWrapper {
        &self.async_fs
    }
    /// The session-lifetime terminal backend, injected into every toolset
    /// re-resolve so background tasks and shell state survive swaps.
    pub(crate) fn terminal_backend(
        &self,
    ) -> &Arc<dyn echo_agent_tools::computer::types::TerminalBackend> {
        self.terminal_backend.backend()
    }
    /// Explicitly shut the session's terminal backend down (kills all of its
    /// child process groups and stops its actor). Called by
    /// `drop_session`/evict so task teardown does not depend on when the last
    /// toolset `Arc` drops.
    pub(crate) fn shutdown_terminal_backend(&self) {
        self.terminal_backend.shutdown();
    }
    /// No-op when the browser tools are compiled out.
    pub(crate) fn shutdown_browser_service(&self) {}
    /// Cancel the workspace-spawned hunk-tracker actor, if this session owns
    /// one. Runs at the session drop chokepoints so the actor (which pins file
    /// contents in `file_states`) stops even while leaked handle clones hold
    /// its channel open.
    pub(crate) fn cancel_hunk_tracker(&self) {
        if let Some(token) = &self.hunk_tracker_cancel {
            token.cancel();
        }
    }
    /// Return the current resolved toolset (snapshot).
    pub fn toolset(&self) -> Arc<FinalizedToolset> {
        self.inner.read().toolset.clone()
    }
    /// Whether the current toolset's `Terminal` resource is the session-owned
    /// backend. `false` means the toolset is externally owned — the local
    /// (shell) mode shape installed by `bind_local_session`, where the shell's
    /// own backend rides the toolset and the session-owned backend is an idle
    /// decoy. Rebuild paths must skip such sessions: finalizing around
    /// [`Self::terminal_backend`] would swap the decoy into the toolset and
    /// detach tools from the shell's live task table. A toolset with no
    /// `Terminal` resource counts as session-owned (nothing to detach).
    pub(crate) async fn toolset_terminal_is_session_owned(&self) -> bool {
        let toolset = self.toolset();
        let res = toolset.resources.lock().await;
        match res.get::<echo_agent_tools::types::resources::Terminal>() {
            Some(t) => Arc::ptr_eq(&t.0, self.terminal_backend()),
            None => true,
        }
    }
    /// Return the current effective tool config baseline.
    pub fn effective_tool_config(&self) -> Arc<ToolServerConfig> {
        self.inner.read().effective_tool_config.clone()
    }
    /// Whether `fingerprint` matches the explicit bind toolset this session
    /// was created (or last rebound) with. `None` = default resolution.
    #[cfg(test)]
    pub(crate) fn bind_tool_config_matches(&self, fingerprint: Option<&serde_json::Value>) -> bool {
        let guard = self
            .bind_tool_config_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.as_ref() == fingerprint
    }
    /// Whether the last snapshot-driven rebuild failed and left the live
    /// toolset stale w.r.t. the current MCP/hub snapshots.
    pub(crate) fn stale_resolve(&self) -> bool {
        self.stale_resolve
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Mark the live toolset stale: a snapshot-driven rebuild failed and the
    /// previous toolset was kept.
    pub(crate) fn mark_stale_resolve(&self) {
        self.stale_resolve
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    /// Clear the stale marker: a freshly resolved toolset was installed.
    pub(crate) fn clear_stale_resolve(&self) {
        self.stale_resolve
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }
    /// Record the explicit bind toolset (or `None` for a default resolution)
    /// this session's toolset was resolved from.
    ///
    /// Unconditional: callers must pair this with the toolset swap under the
    /// session's `update_lock` so fingerprint and live toolset cannot diverge.
    pub(crate) fn set_bind_tool_config_fingerprint(&self, fingerprint: Option<serde_json::Value>) {
        *self
            .bind_tool_config_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = fingerprint;
    }
    /// [`Self::set_bind_tool_config_fingerprint`], but only when no
    /// fingerprint was recorded yet. The `session.bind` create path uses this
    /// (outside `update_lock`): a concurrent rebind can race between session
    /// insertion and this call, swap in its own toolset, and record its
    /// fingerprint under the lock — which this set-if-unset then must not
    /// clobber, or the stored fingerprint would describe a toolset that is no
    /// longer live.
    pub(crate) fn set_bind_tool_config_fingerprint_if_unset(
        &self,
        fingerprint: Option<serde_json::Value>,
    ) {
        let mut guard = self
            .bind_tool_config_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = fingerprint;
        }
    }
    /// Replace both the baseline config and the resolved toolset atomically.
    ///
    /// TOOL-STATE CAVEAT: the outgoing toolset is not flushed here, so an
    /// in-process rebuild can drop up to one debounce window (≤500 ms) of
    /// unpersisted state. Intentionally not "fixed" with a flush-before-rebuild:
    /// tool `call()` does not hold `update_lock`, so a concurrent call would
    /// still race. Restart/snapshot scenarios are unaffected.
    pub(crate) fn replace(
        &self,
        new_effective_tool_config: Arc<ToolServerConfig>,
        new_toolset: Arc<FinalizedToolset>,
    ) {
        let mut w = self.inner.write();
        w.effective_tool_config = new_effective_tool_config;
        w.toolset = new_toolset;
    }
    /// [`Self::replace`], but first carries the session's
    /// `BrowserServiceHandle` from the old toolset into the new one.
    /// Rebuilds produce a fresh `FinalizedToolset`, so the browser service
    /// seeded post-finalize (`finalize_session_setup`) must be carried
    /// forward or the session's live browser state is lost.
    ///
    /// Without the optional browser backend there is no browser service to carry;
    /// only the terminal-orphan diagnostic runs before the swap.
    ///
    /// Callers must hold the session's `update_lock` so the read-then-swap
    /// cannot interleave with another rebuild.
    pub(crate) async fn replace_carrying_browser_service(
        &self,
        new_effective_tool_config: Arc<ToolServerConfig>,
        new_toolset: Arc<FinalizedToolset>,
    ) {
        let old_toolset = self.toolset();
        let old_terminal = {
            let res = old_toolset.resources.lock().await;
            res.get::<echo_agent_tools::types::resources::Terminal>()
                .map(|t| t.0.clone())
        };
        if let Some(old_terminal) = old_terminal
            && !Arc::ptr_eq(&old_terminal, self.terminal_backend())
        {
            crate::handle::WORKSPACE_TERMINAL_BACKEND_ORPHANED_TOTAL
                .with_label_values(&["swap"])
                .inc();
            tracing::error!(
                session_id = %self.session_id,
                "toolset swap: outgoing toolset's terminal backend is not the \
                 session-owned one — its background tasks die with the old toolset"
            );
        }
        self.replace(new_effective_tool_config, new_toolset);
    }
}
/// Sink for delivering a workspace-originated ext-notification (method +
/// params JSON) to the client. The shell installs the concrete delivery:
/// the agent gateway in local mode, the server transport in proxy mode.
pub type ClientExtSink = std::sync::Arc<dyn Fn(String, serde_json::Value) + Send + Sync>;
/// Workspace-wide shared state.
pub struct WorkspaceShared {
    pub(crate) default_tool_config: ToolServerConfig,
    /// Require an explicit toolset on every `session.bind`; see
    /// [`crate::config::WorkspaceConfig::require_explicit_toolset`].
    pub(crate) require_explicit_toolset: bool,
    /// See [`crate::config::WorkspaceConfig::confine_fs_to_workspace_root`].
    /// Default `false`; enabled only for remote-sandbox workspace servers.
    pub(crate) confine_fs_to_workspace_root: bool,
    /// Workspace root directory. Independent of any session — stored
    /// here so it survives session creation/deletion.
    pub(crate) root_cwd: std::path::PathBuf,
    pub(crate) sessions: RwLock<HashMap<String, Arc<WorkspaceSession>>>,
    pub(crate) session_factory: Arc<dyn SessionContextFactory>,
    pub(crate) mcp_tools_snapshot: arc_swap::ArcSwap<Vec<ToolConfig>>,
    pub(crate) events: tokio::sync::broadcast::Sender<echo_agent_workspace_types::WorkspaceEvent>,
    pub(crate) respect_gitignore: bool,
    pub(crate) memory_config: Option<MemoryConfig>,
    pub(crate) hook_registry: Arc<parking_lot::RwLock<echo_agent_hooks::discovery::HookRegistry>>,
    pub(crate) hook_load_errors: Vec<echo_agent_hooks::error::HookError>,
    /// Skill discovery configuration (extra paths, ignore prefixes).
    /// Used by `discover_skills` via the `discovery` module.
    pub(crate) skills_config: crate::discovery::SkillsConfig,
    /// Plugin discovery configuration (CLI dirs, config paths,
    /// disabled/enabled lists). Used by `discover_plugins` via the
    /// `discovery` module.
    pub(crate) plugin_discovery_config: crate::discovery::PluginDiscoveryConfig,
    /// Live server connection handle. `None` until
    /// [`WorkspaceHandle::connect_hub`](crate::handle::WorkspaceHandle::connect_hub)
    /// is called (or if no [`HubConfig`] was provided).
    ///
    /// Uses `tokio::sync::Mutex` so the guard can be held across the
    /// async `HubHandle::connect()` call, preventing TOCTOU races.
    pub(crate) hub_handle: tokio::sync::Mutex<Option<HubHandle>>,
    /// Remote-origin tool configs (consumer direction), updated by the
    /// notification listener.
    pub(crate) hub_tools_snapshot: arc_swap::ArcSwap<Vec<ToolConfig>>,
    /// Server config stashed at construction time for deferred connect.
    pub(crate) hub_config: Option<HubConfig>,
    /// Auth provider for EchoAgent service calls.
    pub(crate) auth_provider: Option<echo_agent_computer_hub_sdk::SharedAuthProvider>,
    /// Connection-level sink feeding the `ActivityTracker` (drained by
    /// `run_activity_feed`); not a network egress. `None` until `connect_hub()` sets it.
    pub(crate) activity_notify_handle:
        arc_swap::ArcSwap<Option<echo_agent_tools::notification::types::ToolNotificationHandle>>,
    /// Sink for workspace-originated ext-notifications to the client (e.g.
    /// `echo.agent/search/fuzzy/status`). Mode-agnostic: the shell wires it to the
    /// agent gateway in local mode, and to the server in proxy mode. `None` until
    /// set via [`WorkspaceHandle::set_client_ext_sink`](crate::handle::WorkspaceHandle::set_client_ext_sink).
    pub(crate) client_ext_sink: arc_swap::ArcSwap<Option<ClientExtSink>>,
    pub(crate) local_registry: echo_agent_computer_hub_sdk::LocalRegistry,
    pub(crate) activity_tracker: std::sync::Arc<crate::activity::ActivityTracker>,
    /// True after the scheduler-liveness poll task started; reconnects must not start a second one.
    pub(crate) scheduler_poll_started: std::sync::atomic::AtomicBool,
    /// Runtime-tunable timing/threshold config for the tool server.
    /// Read by the status publisher task and at shutdown.
    pub(crate) status_config: crate::status_config::StatusConfig,
    /// Opaque metadata for the tool server registration, forwarded verbatim to
    /// the server; structured access goes through
    /// [`WorkspaceShared::server_metadata_typed`].
    pub(crate) server_metadata: Option<serde_json::Value>,
    /// Owner identity, captured at construction; stamps
    /// `workspace_environment.json` and attributes uploads. Empty in test /
    /// local-only contexts.
    pub(crate) identity: crate::upload::environment::WorkspaceIdentity,
    /// Workspace-level fuzzy search manager. Separate from the shell's
    /// own `FuzzySearchManager` — this instance serves remote (hub/RPC)
    /// clients.
    pub(crate) fuzzy_searches:
        std::sync::Arc<tokio::sync::Mutex<crate::file_system::FuzzySearchManager>>,
    pub(crate) lsp: Option<std::sync::Arc<dyn echo_agent_tools::implementations::lsp::LspBackend>>,
    pub(crate) codebase_indexes:
        std::sync::Arc<parking_lot::Mutex<crate::file_system::CodebaseIndexManager>>,
    /// Finalize the FS rewind checkpoint on non-`Completed` turn-end outcomes
    /// (from `ECHO_AGENT_WORKSPACE_REWIND_ALL_OUTCOMES`, default off).
    pub(crate) workspace_rewind_all_outcomes: bool,
    /// Resolved `$ECHO_AGENT_WORKSPACE_HOME` — the workspace-owned on-disk state root
    /// (`<echo_agent_home>/workspace` by default). The upload queue spills here.
    pub(crate) workspace_home: std::path::PathBuf,
    pub(crate) upload_queue: Option<std::sync::Arc<echo_agent_file_utils::queue::UploadQueue>>,
    /// Whether collection is disabled (opt-out, or the fail-closed default).
    pub(crate) data_collection_disabled: bool,
    /// Whether per-session `events.jsonl` recording is enabled
    /// (`ECHO_AGENT_WORKSPACE_EVENTS_ENABLED=true`). When `false`, every
    /// [`session_event_writer`](Self::session_event_writer) hands back an
    /// [`EventWriter::noop()`](echo_agent_session_events::EventWriter::noop) and
    /// no session directory or `events.jsonl` is ever created — the legacy
    /// behaviour, preserved bit-for-bit.
    pub(crate) events_enabled: bool,
    /// Whether per-session `workspace_tool_definitions.json` emission is
    /// enabled (`ECHO_AGENT_WORKSPACE_TOOL_DEFS_ENABLED=true`).
    pub(crate) tool_defs_enabled: bool,
    /// `session_id` → last `ToolsChanged` re-emit `Instant`, debouncing
    /// re-emits per session. The initial bind emission does not consult this map.
    pub(crate) tool_defs_last_emit: dashmap::DashMap<String, std::time::Instant>,
    /// Per-session `events.jsonl` writers, keyed by `session_id`. Lazily opened
    /// on first use under `workspace_home/sessions/{session_id}/`. Held in an
    /// `Arc` shared with [`ActivityTracker`](crate::activity::ActivityTracker) so
    /// `Tool*` events resolve the right writer without a back-reference to
    /// `WorkspaceShared`. Stays empty whenever `events_enabled` is `false`.
    pub(crate) session_event_writers:
        Arc<dashmap::DashMap<String, echo_agent_session_events::EventWriter>>,
    /// In-flight before-turn enqueue tasks, keyed by `(session_id, turn)`.
    /// Stored by `on_before_turn`; evicted on every turn-end path. The `After`
    /// turn-hook handler awaits the handle for its ack's `artifact_count`; the
    /// fire-and-forget path just drops it (detach, not abort).
    pub(crate) inflight_enqueues: dashmap::DashMap<
        (String, u64),
        tokio::task::JoinHandle<echo_agent_file_utils::queue::EnqueueOutcome>,
    >,
    /// Artifact-producer tasks, awaited by the drain and counted by the
    /// status publisher — see
    /// [`WorkspaceHandle::spawn_producer`](crate::handle::WorkspaceHandle).
    pub(crate) producer_tasks: tokio_util::task::TaskTracker,
    /// Bind-time probe-then-mount hook. Default no-op until a command is set.
    pub(crate) bind_mount_hook: arc_swap::ArcSwap<crate::path_virtualization::BindMountHook>,
    /// `(path, size, mtime_ms) → sha256` memo for the client-facing
    /// `workspace.client_fs_*` ops, so unchanged files hash once per
    /// workspace instead of per stat/read.
    /// Test-only seam: runs after the toolset re-resolve returns and before
    /// the post-resolve turn re-check / install in
    /// `resolve_and_swap_session_toolset_locked`, so tests can interleave a
    /// turn start inside the check→install window deterministically.
    #[cfg(test)]
    pub(crate) post_resolve_test_hook: parking_lot::Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    pub(crate) client_fs_hash_memo: crate::file_system::client_fs::FileHashMemo,
}
impl WorkspaceShared {
    /// Workspace root directory.
    pub fn root_cwd(&self) -> &std::path::Path {
        &self.root_cwd
    }
    /// Resolved `$ECHO_AGENT_WORKSPACE_HOME` — the workspace-owned on-disk state root.
    pub fn workspace_home(&self) -> &std::path::Path {
        &self.workspace_home
    }
    /// The durable upload queue used for archives. `None` in tests and
    /// local mode — see
    /// [`WorkspaceShared::upload_queue`].
    pub fn upload_queue(
        &self,
    ) -> Option<&std::sync::Arc<echo_agent_file_utils::queue::UploadQueue>> {
        self.upload_queue.as_ref()
    }
    /// Return the per-session `events.jsonl` writer for `session_id`, opening
    /// (and caching) it on first use under
    /// `workspace_home/sessions/{session_id}/`.
    ///
    /// When `events_enabled` is `false` this returns
    /// [`EventWriter::noop()`](echo_agent_session_events::EventWriter::noop)
    /// WITHOUT touching the cache or the filesystem, so the flag-off path stays
    /// byte-for-byte identical to the legacy behaviour. The returned handle is
    /// `Clone + Send + Sync`; callers emit through it directly.
    pub(crate) fn session_event_writer(
        &self,
        session_id: &str,
    ) -> echo_agent_session_events::EventWriter {
        get_or_open_session_writer(
            self.events_enabled,
            &self.session_event_writers,
            &self.workspace_home,
            session_id,
        )
    }
    /// Like `session_event_writer` but never opens a new writer. Returns `None`
    /// if the session was never opened or already evicted.
    #[allow(dead_code)]
    pub(crate) fn session_event_writer_cached(
        &self,
        session_id: &str,
    ) -> Option<echo_agent_session_events::EventWriter> {
        if !self.events_enabled {
            return None;
        }
        self.session_event_writers
            .get(session_id)
            .map(|w| w.value().clone())
    }
    /// Resolved owner identity of this workspace.
    pub(crate) fn identity(&self) -> &crate::upload::environment::WorkspaceIdentity {
        &self.identity
    }
    /// Stable hub server id (`--server-id`), if a hub config is present.
    pub(crate) fn server_id(&self) -> Option<String> {
        self.hub_config.as_ref().and_then(|c| c.server_id.clone())
    }
    /// Auth provider used for EchoAgent service calls.
    pub fn auth_provider(&self) -> Option<&echo_agent_computer_hub_sdk::SharedAuthProvider> {
        self.auth_provider.as_ref()
    }
    /// Parse the opaque [`server_metadata`](Self::server_metadata) blob into
    /// the typed subset the workspace needs (currently `sandbox_id`);
    /// unknown/missing fields default cleanly. A present-but-malformed blob is
    /// logged and salvaged field-by-field (a bad sibling field must not
    /// silently drop `sandbox_id` from every environment artifact).
    pub(crate) fn server_metadata_typed(&self) -> crate::config::WorkspaceServerMetadata {
        let Some(v) = self.server_metadata.as_ref() else {
            return Default::default();
        };
        match serde_json::from_value(v.clone()) {
            Ok(typed) => typed,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "workspace: malformed server_metadata; salvaging sandbox_id field-wise"
                );
                crate::config::WorkspaceServerMetadata {
                    sandbox_id: v
                        .get("sandbox_id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned),
                    ..Default::default()
                }
            }
        }
    }
    pub fn default_tool_config(&self) -> &ToolServerConfig {
        &self.default_tool_config
    }
    pub fn respect_gitignore(&self) -> bool {
        self.respect_gitignore
    }
    pub fn memory_config(&self) -> Option<&MemoryConfig> {
        self.memory_config.as_ref()
    }
    pub fn mcp_tools_snapshot(&self) -> Arc<Vec<ToolConfig>> {
        self.mcp_tools_snapshot.load_full()
    }
    /// The tool server, if a server connection is active.
    ///
    /// Returns a clone of the [`ToolServer`](echo_agent_computer_hub_sdk::ToolServer)
    /// which is cheap (`Arc` bump). Uses `try_lock` to avoid blocking
    /// on the async mutex from synchronous contexts. Returns `None` if
    /// the lock is held (i.e. a `connect_hub` call is in progress).
    pub fn hub_server(&self) -> Option<echo_agent_computer_hub_sdk::ToolServer> {
        self.hub_handle
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|h| h.server.clone()))
    }
    /// Like [`Self::hub_server`] but awaits the `hub_handle` lock instead of
    /// returning `None` on contention. Use from async contexts that must not
    /// confuse a transient `connect_hub` lock-hold with "no hub connected";
    /// `None` means no hub is connected.
    pub async fn hub_server_blocking(&self) -> Option<echo_agent_computer_hub_sdk::ToolServer> {
        self.hub_handle
            .lock()
            .await
            .as_ref()
            .map(|h| h.server.clone())
    }
    /// Current snapshot of hub-provided tool configs (consumer direction).
    pub fn hub_tools_snapshot(&self) -> Arc<Vec<ToolConfig>> {
        self.hub_tools_snapshot.load_full()
    }
    /// Compose a session's tool `ctx.notification_handle` as a fan-out of the
    /// connection-level activity feed (internal tracker accounting) and the
    /// opt-in per-session `system.notify` sender. Only the `system.notify` leg
    /// reaches a client, so the fan-out can't double-wake. `None` → factory default.
    pub(crate) fn compose_session_notification_handle(
        &self,
        system_notify_handle: Option<ToolNotificationHandle>,
    ) -> Option<ToolNotificationHandle> {
        let activity = self.activity_notify_handle.load_full().as_ref().clone();
        match (activity, system_notify_handle) {
            (None, None) => None,
            (Some(a), None) => Some(a),
            (None, Some(s)) => Some(s),
            (Some(a), Some(s)) => Some(ToolNotificationHandle::tee(vec![a, s])),
        }
    }
    pub fn activity_tracker(&self) -> &std::sync::Arc<crate::activity::ActivityTracker> {
        &self.activity_tracker
    }
    pub fn fuzzy_searches(
        &self,
    ) -> &std::sync::Arc<tokio::sync::Mutex<crate::file_system::FuzzySearchManager>> {
        &self.fuzzy_searches
    }
    pub fn subscribe_events(
        &self,
    ) -> tokio::sync::broadcast::Receiver<echo_agent_workspace_types::WorkspaceEvent> {
        self.events.subscribe()
    }
    pub fn codebase_indexes(
        &self,
    ) -> &std::sync::Arc<parking_lot::Mutex<crate::file_system::CodebaseIndexManager>> {
        &self.codebase_indexes
    }
    /// Skill discovery configuration (extra paths and ignore
    /// prefixes). Used by the `discovery` module when the channel's
    /// `discover_skills` method is called.
    pub fn skills_config(&self) -> &crate::discovery::SkillsConfig {
        &self.skills_config
    }
    /// Plugin discovery configuration (CLI dirs, config paths,
    /// disabled/enabled lists). Used by the `discovery` module when
    /// the channel's `discover_plugins` method is called.
    pub fn plugin_discovery_config(&self) -> &crate::discovery::PluginDiscoveryConfig {
        &self.plugin_discovery_config
    }
    /// Re-resolve every session's toolset and emit `ToolsChanged` events.
    ///
    /// Shared implementation used by `on_mcp_snapshot_changed`,
    /// `on_hub_tools_changed`, and the server notification listener.
    ///
    /// When `use_async_lock` is true, uses `.lock().await` on each
    /// session's `update_lock` (appropriate for spawned async tasks
    /// where notifications must not be silently lost). When false,
    /// uses `try_lock()` and skips sessions whose lock is held.
    pub(crate) async fn re_resolve_all_sessions(
        self: &Arc<Self>,
        source: &str,
        use_async_lock: bool,
    ) -> usize {
        use crate::session::swap_policy::{
            SessionSnapshot, SwapAction, SwapDecision, SwapPolicy, SwapTrigger,
            record_swap_decision,
        };
        let trigger = SwapTrigger::from_rebuild_source(source);
        let mcp_snap = self.mcp_tools_snapshot.load_full();
        let hub_snap = self.hub_tools_snapshot.load_full();
        let sessions: Vec<(String, Arc<WorkspaceSession>)> = {
            let guard = self.sessions.read();
            guard
                .iter()
                .map(|(id, s)| (id.clone(), s.clone()))
                .collect()
        };
        let mut rebuilt = 0usize;
        for (sid, session) in sessions {
            let guard = if use_async_lock {
                session.update_lock.lock().await
            } else {
                match session.update_lock.try_lock() {
                    Ok(g) => g,
                    Err(_) => {
                        tracing::trace!(
                            session = %sid,
                            source = %source,
                            "skipping rebuild: session update_lock held"
                        );
                        continue;
                    }
                }
            };
            let snapshot =
                SessionSnapshot::capture_for_rebuild(&session, &self.activity_tracker).await;
            match SwapPolicy::evaluate(&snapshot, trigger) {
                SwapDecision::Apply => {}
                SwapDecision::Skip(reason) => {
                    record_swap_decision(
                        &self.activity_tracker,
                        trigger,
                        &sid,
                        SwapAction::Skipped(reason),
                    );
                    tracing::warn!(
                        session = %sid,
                        source = %source,
                        "skipping rebuild: toolset terminal backend is externally \
                         owned (local bind)"
                    );
                    drop(guard);
                    continue;
                }
                decision @ (SwapDecision::Reuse | SwapDecision::Defer(_)) => {
                    debug_assert!(
                        false,
                        "snapshot rebuild produced a non-rebuild decision: {decision:?}"
                    );
                    tracing::error!(
                        session = %sid,
                        source = %source,
                        ?decision,
                        "skipping rebuild: snapshot rebuild policy returned a \
                         non-rebuild decision (policy regression)"
                    );
                    drop(guard);
                    continue;
                }
            }
            let baseline = (*session.effective_tool_config()).clone();
            match crate::session::tool_config::resolve_session_toolset_rebuild(
                baseline,
                session.capability_mode(),
                &mcp_snap,
                &hub_snap,
                session.cwd().to_path_buf(),
                session.session_env().clone(),
                &sid,
                self.session_factory.as_ref(),
                Some(self.local_registry.clone()),
                self.lsp.clone(),
                session.viewer_ctx().cloned(),
                self.compose_session_notification_handle(session.system_notify_handle()),
                session.terminal_backend().clone(),
            ) {
                Ok((effective, toolset)) => {
                    session
                        .replace_carrying_browser_service(Arc::new(effective), toolset)
                        .await;
                    session.clear_stale_resolve();
                    record_swap_decision(
                        &self.activity_tracker,
                        trigger,
                        &sid,
                        SwapAction::Applied,
                    );
                    let _ = self.events.send(
                        echo_agent_workspace_types::WorkspaceEvent::ToolsChanged {
                            session_id: sid,
                        },
                    );
                    rebuilt += 1;
                }
                Err(e) => {
                    session.mark_stale_resolve();
                    record_swap_decision(
                        &self.activity_tracker,
                        trigger,
                        &sid,
                        SwapAction::ApplyFailed,
                    );
                    tracing::warn!(
                        session = %sid,
                        source = %source,
                        error = %e,
                        "snapshot rebuild failed for session"
                    );
                }
            }
            drop(guard);
        }
        rebuilt
    }
}
/// Core get-or-open logic for a session's `events.jsonl` writer, factored out of
/// [`WorkspaceShared::session_event_writer`] so the `enabled` gate can be
/// unit-tested without touching process environment.
///
/// - `enabled == false` → [`EventWriter::noop()`]; the `writers` map and the
///   filesystem are left untouched (legacy behaviour preserved).
/// - `enabled == true` → returns the cached writer for `session_id`, opening a
///   fresh one (and creating `workspace_home/sessions/{session_id}/`) on first
///   use. [`EventWriter::open`] uses `create(true).append(true)`, so a writer
///   re-opened for the same directory after a workspace restart APPENDS to the
///   existing `events.jsonl` rather than truncating it.
pub(crate) fn get_or_open_session_writer(
    enabled: bool,
    writers: &dashmap::DashMap<String, echo_agent_session_events::EventWriter>,
    workspace_home: &Path,
    session_id: &str,
) -> echo_agent_session_events::EventWriter {
    use echo_agent_session_events::EventWriter;
    if !enabled {
        return EventWriter::noop();
    }
    if let Some(existing) = writers.get(session_id) {
        return existing.value().clone();
    }
    let sessions_root = workspace_home.join("sessions");
    let dir = sessions_root.join(session_id);
    let create = echo_agent_config::create_dir_all_owner_only(&dir);
    echo_agent_config::set_dir_owner_only(&sessions_root);
    if let Err(e) = create {
        tracing::warn!(
            session_id = %session_id,
            dir = %dir.display(),
            error = %e,
            "failed to create session event dir; events.jsonl disabled for this session (will retry on next use)"
        );
        return EventWriter::noop();
    }
    let writer = EventWriter::open(&dir);
    writers
        .entry(session_id.to_owned())
        .or_insert(writer)
        .clone()
}
#[cfg(test)]
mod tests {
    use super::get_or_open_session_writer;
    use dashmap::DashMap;
    use echo_agent_session_events::{Event, EventWriter};
    fn count_lines(path: &std::path::Path) -> usize {
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .lines()
            .count()
    }
    #[test]
    fn flag_off_returns_noop_and_creates_nothing() {
        let home = tempfile::tempdir().unwrap();
        let writers: DashMap<String, EventWriter> = DashMap::new();
        let w = get_or_open_session_writer(false, &writers, home.path(), "sess-a");
        w.emit(Event::ToolStarted {
            tool_name: "read_file".into(),
        });
        assert!(writers.is_empty(), "flag-off must not cache a writer");
        let sess_dir = home.path().join("sessions").join("sess-a");
        assert!(
            !sess_dir.exists(),
            "flag-off must not create the session dir or events.jsonl"
        );
    }
    /// Session-derived content outside ~/.echo-agent/sessions: same owner-only rule,
    /// including healing a loose pre-existing root from older builds.
    #[cfg(unix)]
    #[test]
    fn flag_on_creates_owner_only_session_event_dir() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let writers: DashMap<String, EventWriter> = DashMap::new();
        let root = home.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = get_or_open_session_writer(true, &writers, home.path(), "sess-perm");
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode(&root.join("sess-perm")),
            0o700,
            "session event dir must be 0700"
        );
        assert_eq!(mode(&root), 0o700, "loose sessions root must heal to 0700");
    }
    #[test]
    fn flag_on_opens_and_writes_real_content() {
        let home = tempfile::tempdir().unwrap();
        let writers: DashMap<String, EventWriter> = DashMap::new();
        let w = get_or_open_session_writer(true, &writers, home.path(), "sess-b");
        w.emit(Event::YoloToggled { enabled: true });
        assert_eq!(writers.len(), 1, "flag-on must cache the opened writer");
        let path = home
            .path()
            .join("sessions")
            .join("sess-b")
            .join("events.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(v["type"], "yolo_toggled");
        assert_eq!(v["enabled"], true);
        assert!(v["ts"].as_str().is_some());
    }
    #[test]
    fn second_call_reuses_one_cache_entry() {
        let home = tempfile::tempdir().unwrap();
        let writers: DashMap<String, EventWriter> = DashMap::new();
        get_or_open_session_writer(true, &writers, home.path(), "sess-c").emit(
            Event::ToolStarted {
                tool_name: "a".into(),
            },
        );
        get_or_open_session_writer(true, &writers, home.path(), "sess-c").emit(
            Event::ToolStarted {
                tool_name: "b".into(),
            },
        );
        assert_eq!(writers.len(), 1, "same session must reuse one cache entry");
        let path = home
            .path()
            .join("sessions")
            .join("sess-c")
            .join("events.jsonl");
        assert_eq!(count_lines(&path), 2);
    }
    #[test]
    fn reopen_after_restart_appends() {
        let home = tempfile::tempdir().unwrap();
        {
            let writers: DashMap<String, EventWriter> = DashMap::new();
            get_or_open_session_writer(true, &writers, home.path(), "sess-d").emit(
                Event::ToolStarted {
                    tool_name: "before-restart".into(),
                },
            );
        }
        {
            let writers: DashMap<String, EventWriter> = DashMap::new();
            get_or_open_session_writer(true, &writers, home.path(), "sess-d").emit(
                Event::ToolStarted {
                    tool_name: "after-restart".into(),
                },
            );
        }
        let path = home
            .path()
            .join("sessions")
            .join("sess-d")
            .join("events.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.trim().lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "re-open after restart must append, preserving the earlier line"
        );
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["tool_name"], "before-restart");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["tool_name"], "after-restart");
    }
}
