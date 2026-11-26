#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
//! Core workspace library: FS, VCS, permissions, tool config, and subsystem wiring.
pub mod activity;
pub mod capability;
pub mod channel;
pub mod config;
pub mod discovery;
pub mod envrc;
pub mod error;
pub mod export_github;
pub mod file_system;
pub mod folder_trust;
pub mod fs_notify;
pub(crate) mod git_odb;
pub mod handle;
pub mod hub;
pub mod hub_auth;
pub mod hub_channel;
pub mod hub_ids;
pub mod hub_server;
pub mod image_capabilities;
pub mod mcp;
pub(crate) mod path_virtualization;
pub mod permission;
pub mod project_config;
pub mod publish;
pub mod recovery;
mod restore_fetch;
pub mod scheduler_liveness;
pub use restore_fetch::{EnsureCommitsOutcome, ensure_commits_reachable};
pub use session::git::git_object_exists;
pub mod rpc_envelope;
pub mod session;
pub mod status_config;
pub(crate) mod telemetry;
pub use status_config::{ProactiveRefreshConfig, StatusConfig};
pub mod trust;
pub(crate) mod upload;
pub mod util;
pub mod workspace_ops;
pub mod worktree;
pub use capability::CapabilityMode;
pub use channel::{TransportCallResult, TransportContext, TransportError, TransportNotification};
pub use config::{
    AgentSessionConfig, DEFAULT_EVENT_BUFFER_CAPACITY, HookSourceConfig, IsolationMode,
    MemoryConfig, SessionContextFactory, SessionTerminalBackend, WorkspaceConfig,
};
pub use echo_agent_hunk_tracker::HunkTrackerHandle;
pub use echo_agent_workspace_client::WorkspaceClient;
pub use echo_agent_workspace_types::WorkspaceEvent;
pub use error::{WorkspaceError, WorkspaceResult};
pub use file_system::*;
pub use handle::{
    DrainOutcome, DrainReason, WorkspaceHandle, connect_local_workspace, resolve_workspace_home,
    termination_grace_from_env,
};
pub use hub::HubConfig;
pub use path_virtualization::{
    ARTIFACTS_ALIAS, BindLifecycleCtx, BindMountError, BindMountHook, PathVirtualization,
    VISIBLE_ROOT,
};
pub use permission::*;
pub use session::{WorkspaceSession, WorkspaceShared};
pub use session::{file_state, git, jj};
pub use upload::environment::{WorkspaceEnvironment, WorkspaceIdentity};
pub use workspace_ops::{WorkspaceOp, WorkspaceOps};
/// Zero-init every workspace metric family so idle panels render a `0` baseline
/// instead of "No data". Idempotent; call once at workspace-server startup.
pub fn init_metrics() {
    handle::init_metrics();
    recovery::init_metrics();
    session::swap_policy::init_metrics();
    upload::init_metrics();
    permission::init_metrics();
    hub_server::init_metrics();
    hub_auth::init_metrics();
}
/// Crate-wide lock serializing every test that mutates the process-global
/// environment (`ECHO_AGENT_HOME`, `HOME`, …). nextest isolates each test in its own
/// process, but `cargo test --lib` shares ONE process across threads, so
/// per-module locks don't serialize cross-module — a peer test in another module
/// can clobber `ECHO_AGENT_HOME` mid-test. A single shared lock (used by every
/// env-mutating test module) is required for that single-process run to be
/// race-free.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// Crate-shared RAII guard for a single process env var in tests: sets (or
/// unsets) it on construction and restores the prior value on drop. The ONE
/// generic env-var guard for the whole crate (replaces the per-module copies).
///
/// Hold it together with [`ENV_TEST_LOCK`] for the test's lifetime, acquiring
/// the lock FIRST so it drops LAST — the env restore (this guard) then runs
/// before the lock releases, so no peer test observes the temporary value.
#[cfg(test)]
pub(crate) struct TestEnvGuard {
    key: &'static str,
    prev: Option<std::ffi::OsString>,
}
#[cfg(test)]
impl TestEnvGuard {
    /// Set `key` to `val`, restoring the prior value on drop.
    pub(crate) fn set(key: &'static str, val: &std::path::Path) -> Self {
        let prev = std::env::var_os(key);
        unsafe { std::env::set_var(key, val) };
        Self { key, prev }
    }
    /// Unset `key`, restoring the prior value on drop.
    pub(crate) fn unset(key: &'static str) -> Self {
        let prev = std::env::var_os(key);
        unsafe { std::env::remove_var(key) };
        Self { key, prev }
    }
}
#[cfg(test)]
impl Drop for TestEnvGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(prev) => unsafe { std::env::set_var(self.key, prev) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}
/// Holds [`ENV_TEST_LOCK`] AND a set of [`TestEnvGuard`]s as ONE value, so a
/// test (or fixture) can return/bind it however it likes and still be correct.
///
/// Struct fields drop in DECLARATION order, so `_env` (declared first) restores
/// every env var BEFORE `_lock` (declared last) releases the lock — making the
/// "restore before unlock" invariant compile-enforced, not convention-dependent
/// (a single `let _ = LockedTestEnv::lock()…;` binding can't reorder it).
///
/// Acquire the lock first via [`lock`](Self::lock), then mutate env under it with
/// the chained [`set`](Self::set) builder.
#[cfg(test)]
pub(crate) struct LockedTestEnv {
    _env: Vec<TestEnvGuard>,
    _lock: std::sync::MutexGuard<'static, ()>,
}
#[cfg(test)]
impl LockedTestEnv {
    /// Acquire [`ENV_TEST_LOCK`] (held until this value drops).
    pub(crate) fn lock() -> Self {
        Self {
            _env: Vec::new(),
            _lock: ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }
    /// Set `key` to `val` under the held lock, restoring the prior value on drop.
    ///
    /// Intended for DISTINCT keys; the restore order across repeated `set`s of
    /// the SAME key is unspecified (guards restore in insertion order).
    pub(crate) fn set(mut self, key: &'static str, val: &std::path::Path) -> Self {
        self._env.push(TestEnvGuard::set(key, val));
        self
    }
}
#[cfg(test)]
mod init_metrics_tests {
    /// `init_metrics()` is idempotent (a double call must not panic on
    /// re-register) and populates a `0` baseline series for each family so
    /// panels render `0` instead of "No data".
    #[test]
    fn init_metrics_is_idempotent_and_registers_baselines() {
        super::init_metrics();
        super::init_metrics();
        let families = prometheus::gather();
        let has = |name: &str, want: &[(&str, &str)]| {
            families
                .iter()
                .filter(|mf| mf.name() == name)
                .flat_map(|mf| mf.get_metric())
                .any(|m| {
                    want.iter().all(|(k, v)| {
                        m.get_label()
                            .iter()
                            .any(|l| l.name() == *k && l.value() == *v)
                    })
                })
        };
        assert!(has(
            "echo_agent_workspace_upload_outcome_total",
            &[("phase", "tool_state"), ("outcome", "succeeded")]
        ));
        assert!(has(
            "echo_agent_workspace_rpc_requests_total",
            &[("method", "unknown"), ("result", "error")]
        ));
        assert!(has(
            "echo_agent_workspace_rpc_errors_total",
            &[("method", "unknown"), ("error_kind", "unknown_method")]
        ));
        for stage in [
            "startup_recovery",
            "tool_catalog",
            "hub_ws_connect",
            "connect_hub",
            "time_to_ready",
        ] {
            for outcome in ["ok", "error"] {
                assert!(
                    has(
                        "echo_agent_workspace_startup_stage_duration_seconds",
                        &[("stage", stage), ("outcome", outcome)]
                    ),
                    "missing baseline stage={stage} outcome={outcome}"
                );
            }
        }
        assert!(has(
            "echo_agent_workspace_drain_started_total",
            &[("reason", "sigterm")]
        ));
        assert!(has(
            "echo_agent_workspace_toolset_swap_rejected_total",
            &[("reason", "turn_active"), ("trigger", "update_tool_config")]
        ));
        for outcome in [
            "ok",
            "failed_retry",
            "failed_exhausted",
            "failed_terminal",
            "skipped_disabled",
        ] {
            assert!(
                has(
                    "echo_agent_workspace_oidc_proactive_refresh_total",
                    &[("outcome", outcome)]
                ),
                "missing oidc refresh baseline outcome={outcome}"
            );
        }
        assert!(has(
            "echo_agent_workspace_orphan_lost_total",
            &[("reason", "sha_mismatch")]
        ));
        assert!(
            families
                .iter()
                .any(|mf| mf.name() == "echo_agent_workspace_env_capture_panic_total")
        );
        assert!(
            families
                .iter()
                .any(|mf| mf.name() == "echo_agent_workspace_permission_timeout_total")
        );
    }
}
