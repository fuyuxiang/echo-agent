//! `AuthManager` -- single source of truth for `auth.json` + the
//! in-memory bearer cache. Mutations go through `refresh_chain` or `update`; lock
//! and enrichment helpers live in submodules.

use chrono::{Duration, Utc};
use echo_agent_auth::bearer_suffix;
use parking_lot::RwLock;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use tokio_util::sync::CancellationToken;

#[path = "manager/enrichment.rs"]
mod enrichment;
#[path = "manager/lock.rs"]
pub(super) mod lock;
#[path = "manager/remedy.rs"]
mod remedy;
pub(crate) use remedy::{AuthRemedy, BoundedRefresh, SilentRefresh};
#[path = "manager/refresh_chain.rs"]
mod refresh_chain;
#[path = "manager/sleep_gate.rs"]
mod sleep_gate;

use lock::{LockAcquire, try_lock_auth_file_async};
use sleep_gate::SleepGate;

use crate::util::dual_clock::DualClock;

use crate::auth::config::EchoAgentComConfig;
use crate::auth::error::AuthError;
use crate::auth::token_type::TokenType;
use echo_agent_telemetry::events::ManualAuthSurface;

#[cfg(test)]
use super::model::UserInfo;
use super::model::{
    AuthMode, EchoAgentAuth, early_invalidation, is_expired, is_expired_with_buffer, lookup_auth,
};
use super::refresh::{RefreshOutcome, TokenRefresher, resolve_refresh_credential};
use super::storage::{
    AuthFileLock, read_auth_json, read_auth_json_or_empty_recovering_corrupt, write_auth_json,
};

#[cfg(test)]
use super::storage::read_auth_json_or_empty;
#[cfg(test)]
use chrono::DateTime;
#[cfg(test)]
use enrichment::apply_user_info_enrichment;

#[cfg(test)]
use super::model::AuthStore;
use super::model::LEGACY_SCOPE;

/// Why a token refresh is being requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshReason {
    /// Pre-request check. Return cached token if still valid.
    PreRequest,
    /// Server returned 401/403. Must obtain a different token.
    ServerRejected,
}

/// Why [`AuthManager::try_use_disk_token`] — the single enforcement point
/// for disk-token adoption — declined a disk token. Names the decision
/// instead of collapsing every decline into a bare `None`, so callers can
/// carry it into the structured log and tests can assert the exact guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiskTokenDecline {
    /// No token on disk for this scope (or `auth.json` was unreadable).
    Missing,
    /// The disk token is expired (buffer-inclusive, like every adopt path).
    Expired,
    /// The disk token was minted before the live in-memory one (beyond skew
    /// tolerance): disk lagging memory (`update()` keeps a successful mint
    /// in memory when its disk write fails), not a sibling rotation.
    LaggingMemoryMint,
    /// `ServerRejected` only: the disk key matches the rejected bearer, so
    /// no sibling has refreshed yet.
    SameKeyAsRejected,
}

impl DiskTokenDecline {
    /// Stable name for structured-log payloads.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Expired => "expired",
            Self::LaggingMemoryMint => "lagging_memory_mint",
            Self::SameKeyAsRejected => "same_key_as_rejected",
        }
    }
}

/// Timeout for acquiring the advisory `auth.json.lock` file lock.
/// Used by advisory (non-critical) lock sites: `flow.rs`, `enrichment.rs`,
/// `recovery.rs`.
pub(crate) const AUTH_LOCK_TIMEOUT: StdDuration = StdDuration::from_secs(10);

/// Lock timeout for `refresh_chain`, held across the IdP call to prevent
/// refresh-token reuse. Sized against the OIDC exchange that actually holds
/// the flock: one refresh POST gets a 15s HTTP budget with up to two retries
/// (`refresh_retry_policy` in `auth/oidc/protocol.rs`), plus discovery and
/// JWKS fetches on a cold cache. A healthy single attempt fits with margin;
/// a degraded IdP running the full retry ladder does not — followers then go
/// transient at 25s + `LOCK_TIMEOUT_WAIT` instead of waiting the ladder out.
/// Deliberate tradeoff: adopt-or-transient is the design, so a follower that
/// cannot adopt a sibling's mint retries on its caller's backoff rather than
/// pinning startup-path callers behind a slow leader (pre-fix: 45s waits).
pub(crate) const REFRESH_LOCK_TIMEOUT: StdDuration = StdDuration::from_secs(25);

/// Budget for [`AuthManager::refresh_chain_bounded`] at RPC-path call sites:
/// covers one full healthy OIDC token attempt (15s HTTP budget) plus flock
/// acquisition margin, while staying below `REFRESH_LOCK_TIMEOUT`.
pub(crate) const BEST_EFFORT_REFRESH_TIMEOUT: StdDuration = StdDuration::from_secs(20);
const _: () = assert!(
    BEST_EFFORT_REFRESH_TIMEOUT.as_millis() < REFRESH_LOCK_TIMEOUT.as_millis(),
    "an RPC-path bounded refresh must never wait out a full lock convoy"
);
// Defense-in-depth pin of the pager's default 30s startup connect gate
// (30_000; not importable here): startup awaits are separately bounded well
// below it, but this keeps the flock budget from silently regrowing past the
// gate the field incident died on.
const _: () = assert!(
    REFRESH_LOCK_TIMEOUT.as_millis() + LOCK_TIMEOUT_WAIT.as_millis() < 30_000,
    "one lock acquisition attempt plus LOCK_TIMEOUT_WAIT must fit the pager's default startup gate"
);

/// Long poll interval used by the proactive refresh task when no
/// productive refresh is possible (see [`compute_proactive_sleep`]).
/// Long enough to avoid CPU/log spam; short enough that a `hot_swap()`
/// or `configure_refresher()` is picked up in a reasonable window.
pub(crate) const BACKOFF_INTERVAL: StdDuration = StdDuration::from_secs(300);

/// How long to wait after a file lock timeout before re-reading disk,
/// giving the lock holder time to finish writing.
const LOCK_TIMEOUT_WAIT: StdDuration = StdDuration::from_secs(2);

/// Maximum random jitter (seconds) added to the proactive refresh sleep
/// to stagger sibling processes and avoid thundering-herd IdP calls.
const JITTER_RANGE_SECS: i64 = 60;

/// `force_reload_from_disk` re-read budget. A single `auth.json` read can
/// return `NotFound`/unreadable for reasons unrelated to logout — most
/// notably the first read right after wake-from-sleep, where the filesystem
/// briefly resolves the path to `ENOENT`. Retrying a few times absorbs that
/// transient; a genuine deletion/logout stays missing across the budget.
const RELOAD_RETRY_TRIES: usize = 3;

/// Backoff between `force_reload_from_disk` re-reads. Short enough to keep the
/// (sync) caller responsive, long enough to outlast a wake-time FS settle.
/// Only paid on the disk-anomaly branch, never on a healthy read.
const RELOAD_RETRY_BACKOFF: StdDuration = StdDuration::from_millis(50);

/// Sticky permanent-refresh verdict, scoped to the credential that produced it
/// (`token_key`). The scope is what makes invalidation automatic: any other
/// credential reads through as "no failure", so no manual clearing is needed.
struct ScopedRefreshFailure {
    token_key: String,
    error: crate::auth::error::RefreshTokenFailedError,
    /// Two-clock timestamp (see [`DualClock`]): the TTL below is *real* time,
    /// so it must keep counting across a system sleep. The monotonic clock
    /// pauses during suspend — with it alone, a failure cached just before
    /// sleep would still short-circuit `auth()` for a further
    /// [`PERMANENT_FAILURE_TTL`] of *awake* time after wake, exactly when the
    /// user comes back and expects a recovered session.
    recorded_at: DualClock,
}

/// Auto-expiry safety net for the recoverable reasons (`ClientRejected`,
/// `Other`): they self-heal without re-login even if the credential never
/// changes. `RefreshTokenRejected` is excluded (see `is_sticky`). Independent
/// of `BACKOFF_INTERVAL` (equal value is coincidental). Measured on both
/// clocks — expires once *either* the monotonic or the wall clock passes the
/// bound, so it means "5 real minutes", not "5 awake minutes" (a suspend
/// doesn't extend it).
const PERMANENT_FAILURE_TTL: StdDuration = StdDuration::from_secs(300);

/// Single source of truth for `auth.json` + the in-memory bearer.
///
/// Lock order: `refresh_lock` (async) -> the sync locks (`inner` / `refresher`
/// / `permanent_failure` / `manual_auth`), never co-held; `permanent_failure()`
/// reads `permanent_failure` first and only then `inner` (via
/// `attempted_verdict_key`, when a verdict is stored), never co-held. Never hold
/// a `parking_lot` guard across `.await`. Refreshers return [`RefreshOutcome`]
/// for `refresh_chain` to apply.
/// Redacted `Debug` so `AuthManager` (held via `Arc` inside `Debug`-derived
/// types like `PersistenceMsg`) never leaks credentials into logs or panics.
impl std::fmt::Debug for AuthManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthManager").finish_non_exhaustive()
    }
}

pub struct AuthManager {
    /// In-memory bearer. Mutate via [`Self::with_inner_write`] or
    /// [`Self::refresh_chain`]; the closure helpers' sync return type
    /// enforces "no `.await` while holding the lock". `Arc`
    /// so the spawned `/user` enrichment task can write back.
    inner: Arc<RwLock<Option<EchoAgentAuth>>>,
    path: PathBuf,
    scope: String,
    echo_agent_com_config: EchoAgentComConfig,
    proxy_base_url: String,
    refresher: RwLock<Option<Arc<dyn TokenRefresher>>>,
    /// Idempotency guard for `configure_refresher` so double-calls
    /// don't reset internal state (e.g. `OidcRefresher::upload_in_flight`).
    refresher_configured: std::sync::atomic::AtomicBool,
    /// Idempotency guard for `start_proactive_refresh` so we don't
    /// spawn competing refresh loops on the same Arc.
    proactive_started: std::sync::atomic::AtomicBool,
    /// Serializes concurrent refresh attempts (async, held across .await).
    refresh_lock: tokio::sync::Mutex<()>,
    permanent_failure: RwLock<Option<ScopedRefreshFailure>>,
    /// Loop-body iteration count -- catches busy-loops where the
    /// back-off gate fails to fire.
    #[cfg(test)]
    proactive_iter_count: std::sync::atomic::AtomicU32,
    /// `tokio::spawn` count -- catches idempotency-guard regressions
    /// (orthogonal to `proactive_iter_count`).
    #[cfg(test)]
    proactive_starts: std::sync::atomic::AtomicU32,
    /// Notified after every successful token refresh (key changed).
    /// Used by `ModelsManager` to trigger model catalog recovery
    /// after sleep/wake without relying on the file watcher.
    refresh_notify: Arc<tokio::sync::Notify>,
    /// Notified on every OS wake (`DidWake`), including dark wakes. Re-arms
    /// the proactive-refresh loop, whose monotonic sleep pauses during
    /// suspend — a pre-sleep schedule would otherwise fire hours of
    /// awake-time late, leaving post-wake requests to discover the expired
    /// token via 401s. See `start_proactive_refresh`.
    wake_notify: tokio::sync::Notify,
    /// Last state `read_disk_auth` observed for this manager's scope.
    /// Drives transition-level unified logging: hot retry loops read the
    /// disk every few seconds, so per-read logging would flood and no
    /// logging leaves auth.json loss invisible in production captures.
    disk_state: RwLock<Option<DiskAuthState>>,
    /// See [`Self::cached_disk_api_key`].
    static_key_cache: parking_lot::Mutex<Option<StaticKeyCacheEntry>>,
    /// Model `api_key` / resolved `env_key` for voice/tools without a session.
    /// Not a session token (those live on `inner`). Prefers over disk; env wins.
    process_static_api_key: parking_lot::RwLock<Option<String>>,
    sleep_gate: SleepGate,
    /// Count of in-flight IdP refreshes (the network call only), so a
    /// sleep-imminent transition can wait for a refresh straddling suspend to
    /// finish before acknowledging sleep. Maintained by [`InFlightGuard`].
    refresh_in_flight: std::sync::atomic::AtomicU32,
    /// Pairs with `refresh_drain_cv` to let `set_system_sleep_imminent` (called
    /// on the OS power-listener thread) block until `refresh_in_flight` reaches
    /// zero. A plain `Mutex`/`Condvar` rather than the async `refresh_notify`
    /// because the power callback is synchronous and runs off any runtime.
    refresh_drain_lock: parking_lot::Mutex<()>,
    /// Condvar signaled by [`InFlightGuard::drop`] when the in-flight count hits
    /// zero; waited on by `hold_sleep_ack_until_refresh_drains`.
    refresh_drain_cv: parking_lot::Condvar,
    /// Idempotency guard for `start_system_power_listener`.
    power_listener_started: std::sync::atomic::AtomicBool,
    /// Keeps the OS power listener alive for this manager's lifetime; dropping
    /// it stops the listener. `None` until started (or if unavailable).
    power_listener: parking_lot::Mutex<Option<echo_agent_system_power::SystemPowerListener>>,
    /// Per-process `manual_auth` KPI debounce, shared by all recoveries on this
    /// manager so repeated 401s on the most-recent dead credential emit once.
    manual_auth: crate::auth::recovery::ManualAuthTracker,
    /// First-party env key may advertise after initialize probe (default true).
    /// Lives here (not on `MvpAgent`) so the probe verdict is auth-owned.
    first_party_env_api_key_ok: std::sync::atomic::AtomicBool,
    /// When the current unbroken run of dark-wake refresh deferrals began, on
    /// two clocks (see [`DualClock`]); `None` outside such a run. Bounds the
    /// deferral to [`sleep_gate::DARK_WAKE_DEFER_MAX`] so a machine stuck
    /// reporting dark wake can't defer refresh forever — see
    /// [`AuthManager::should_defer_for_dark_wake`].
    dark_wake_defer_since: parking_lot::RwLock<Option<DualClock>>,
    /// Test-only override for [`AuthManager::is_dark_wake`]. `Some(_)` forces
    /// the dark-wake decision so the refresh-deferral path is unit-testable
    /// without a real macOS dark wake. `None` = consult the OS.
    #[cfg(test)]
    dark_wake_override: parking_lot::Mutex<Option<bool>>,
    /// Test-only override for [`AuthManager::is_devbox_environment`]. CI runs in
    /// K8s pods where the real check is `true`, which would otherwise let
    /// `DevboxRecovery` adopt a seeded valid token; `Some(_)` pins the result.
    #[cfg(test)]
    devbox_override: parking_lot::Mutex<Option<bool>>,
}

/// Discriminated outcome of a disk read, for transition logging.
/// `Ok` = entry present (possibly expired); the rest explain *why*
/// `read_disk_auth` returned `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiskAuthState {
    /// auth.json readable and the scope entry exists.
    Ok,
    /// auth.json does not exist.
    FileMissing,
    /// auth.json readable but has no usable entry for this scope
    /// (scope removed, or only a skipped legacy WebLogin entry).
    EntryMissing,
    /// auth.json exists but could not be read (corrupt JSON, permission
    /// or I/O error).
    Unreadable,
}

/// On-disk outcome of [`AuthManager::remove_scope_impl`], emitted as the
/// `disk_mutation` field of the `auth: scope removed from auth.json` event so a
/// deliberate removal stays distinguishable from accidental credential loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeRemoval {
    /// Scope entry dropped; other scopes remain.
    EntryRemoved,
    /// Last scope dropped; auth.json deleted.
    FileDeleted,
    /// Lock unavailable (held by another process); disk left untouched.
    SkippedLockUnavailable,
    /// Lock held but auth.json was unreadable; disk left untouched.
    SkippedUnreadable,
}

impl ScopeRemoval {
    /// Stable telemetry label for the `disk_mutation` field.
    fn label(self) -> &'static str {
        match self {
            Self::EntryRemoved => "entry removed",
            Self::FileDeleted => "file deleted (no scopes left)",
            Self::SkippedLockUnavailable => "skipped (lock unavailable)",
            Self::SkippedUnreadable => "skipped (auth.json unreadable)",
        }
    }
}

// ── Construction + builders ──────────────────────────────────────────

impl AuthManager {
    pub fn new(echo_agent_home: &Path, echo_agent_com_config: EchoAgentComConfig) -> Self {
        let scope = echo_agent_com_config.auth_scope();
        let proxy_base_url =
            crate::agent::config::EndpointsConfig::from_effective_config().proxy_url();

        echo_agent_telemetry::unified_log::info(
            "AuthManager::new",
            None,
            Some(serde_json::json!({
                "scope": &scope,
                "echo_agent_home": echo_agent_home.display().to_string(),
                "HOME": std::env::var("HOME").unwrap_or_else(|_| "(unset)".into()),
                "ECHO_AGENT_HOME": std::env::var("ECHO_AGENT_HOME").unwrap_or_else(|_| "(unset)".into()),
                "ECHO_AGENT_AUTH_PATH": std::env::var("ECHO_AGENT_AUTH_PATH").unwrap_or_else(|_| "(unset)".into()),
                "ECHO_AGENT_AUTH": std::env::var("ECHO_AGENT_AUTH").map(|_| "(set)".to_string()).unwrap_or_else(|_| "(unset)".into()),
            })),
        );

        // ECHO_AGENT_AUTH_PATH: custom file path (overrides default $ECHO_AGENT_HOME/auth.json).
        // Resolved before the ECHO_AGENT_AUTH branch so inline-credential managers
        // also honor it: their later refresh persistence (`update()`) writes to
        // this path, and previously the inline branch hardcoded the default —
        // silently splitting reads (inline) from writes (default path).
        let isolated = echo_agent_config::controlled_discovery_home().is_some();
        let path = (!isolated).then(|| std::env::var("ECHO_AGENT_AUTH_PATH").ok()).flatten()
            .map(PathBuf::from)
            .unwrap_or_else(|| echo_agent_home.join("auth.json"));

        // ECHO_AGENT_AUTH: inline JSON credentials (highest priority, read-only).
        if let Some(inline_json) = (!isolated).then(|| std::env::var("ECHO_AGENT_AUTH").ok()).flatten() {
            if let Ok(auth) = serde_json::from_str::<EchoAgentAuth>(&inline_json) {
                return Self::assemble(
                    Some(auth),
                    path,
                    scope,
                    echo_agent_com_config,
                    proxy_base_url,
                    None,
                );
            }
            tracing::warn!("ECHO_AGENT_AUTH set but failed to parse as JSON, falling back to file");
        }

        let (auth, auth_read_detail, initial_disk_state) = match read_auth_json(&path) {
            Ok(map) => {
                let found = lookup_auth(&map, &scope);
                // If lookup_auth skipped a legacy WebLogin token, remove the
                // stale scope entry from auth.json so it is not re-evaluated
                // on every launch.
                if found.is_none()
                    && map
                        .get(LEGACY_SCOPE)
                        .is_some_and(|a| a.auth_mode == AuthMode::WebLogin)
                {
                    // Best-effort cleanup under advisory lock (consistent with
                    // other auth.json writers). Non-blocking: if the lock is
                    // held by a concurrent process, skip — retried next launch.
                    if let Some(_lock) = lock::try_lock_auth_file_nonblocking(&path) {
                        let mut cleaned = map.clone();
                        cleaned.remove(LEGACY_SCOPE);
                        let _ = write_auth_json(&path, &cleaned);
                        tracing::debug!("auth: removed stale WebLogin scope from auth.json");
                        // lock released on drop
                    } else {
                        tracing::debug!("auth: skipped WebLogin cleanup (lock unavailable)");
                    }
                }
                let detail = serde_json::json!({
                    "read": "ok",
                    "resolved_path": path.display().to_string(),
                    "scopes_on_disk": map.keys().collect::<Vec<_>>(),
                    "target_scope": &scope,
                    "found": found.is_some(),
                    "auth_mode": found.as_ref().map(|a| format!("{:?}", a.auth_mode)),
                    "is_expired": found.as_ref().map(is_expired),
                    "key_prefix": found.as_ref().map(|a| bearer_suffix(&a.key).to_owned()),
                });
                let state = if found.is_some() {
                    DiskAuthState::Ok
                } else {
                    DiskAuthState::EntryMissing
                };
                (found, detail, state)
            }
            Err(e) => {
                let detail = serde_json::json!({
                    "read": "error",
                    "error": e.to_string(),
                    "path": path.display().to_string(),
                    "path_exists": path.exists(),
                });
                let state = if e.kind() == std::io::ErrorKind::NotFound {
                    DiskAuthState::FileMissing
                } else {
                    DiskAuthState::Unreadable
                };
                (None, detail, state)
            }
        };
        echo_agent_telemetry::unified_log::info(
            "AuthManager::new auth.json load result",
            None,
            Some(auth_read_detail),
        );

        let manager = Self::assemble(
            auth,
            path,
            scope,
            echo_agent_com_config,
            proxy_base_url,
            Some(initial_disk_state),
        );
        // Clear a wrong-team session left on disk before the pin was deployed,
        // so the first launch forces a compliant login.
        manager.enforce_pin_on_loaded_token();
        manager
    }

    /// Single field-assembly point for [`Self::new`]'s two construction paths
    /// (inline `ECHO_AGENT_AUTH` vs. on-disk `auth.json`), which differ only in the
    /// threaded fields. One literal means a newly added field can't be silently
    /// dropped from one branch.
    fn assemble(
        inner: Option<EchoAgentAuth>,
        path: PathBuf,
        scope: String,
        echo_agent_com_config: EchoAgentComConfig,
        proxy_base_url: String,
        disk_state: Option<DiskAuthState>,
    ) -> Self {
        Self {
            inner: Arc::new(RwLock::new(inner)),
            path,
            scope,
            echo_agent_com_config,
            proxy_base_url,
            refresher: RwLock::new(None),
            refresher_configured: std::sync::atomic::AtomicBool::new(false),
            proactive_started: std::sync::atomic::AtomicBool::new(false),
            refresh_lock: tokio::sync::Mutex::new(()),
            permanent_failure: RwLock::new(None),
            #[cfg(test)]
            proactive_iter_count: std::sync::atomic::AtomicU32::new(0),
            #[cfg(test)]
            proactive_starts: std::sync::atomic::AtomicU32::new(0),
            refresh_notify: Arc::new(tokio::sync::Notify::new()),
            wake_notify: tokio::sync::Notify::new(),
            disk_state: RwLock::new(disk_state),
            static_key_cache: parking_lot::Mutex::new(None),
            process_static_api_key: parking_lot::RwLock::new(None),
            sleep_gate: SleepGate::default(),
            refresh_in_flight: std::sync::atomic::AtomicU32::new(0),
            refresh_drain_lock: parking_lot::Mutex::new(()),
            refresh_drain_cv: parking_lot::Condvar::new(),
            power_listener_started: std::sync::atomic::AtomicBool::new(false),
            power_listener: parking_lot::Mutex::new(None),
            manual_auth: Default::default(),
            first_party_env_api_key_ok: std::sync::atomic::AtomicBool::new(true),
            dark_wake_defer_since: parking_lot::RwLock::new(None),
            #[cfg(test)]
            dark_wake_override: parking_lot::Mutex::new(None),
            #[cfg(test)]
            devbox_override: parking_lot::Mutex::new(None),
        }
    }

    /// Whether initialize's first-party env-key probe still allows advertising.
    pub(crate) fn first_party_env_api_key_ok(&self) -> bool {
        self.first_party_env_api_key_ok
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Record initialize probe result for cached-token fallthrough advertise.
    pub(crate) fn set_first_party_env_api_key_ok(&self, ok: bool) {
        self.first_party_env_api_key_ok
            .store(ok, std::sync::atomic::Ordering::Relaxed);
    }

    /// Clear the disk-loaded token if it violates the team pin (startup only;
    /// the read/dispense gates cover everything cached afterwards).
    fn enforce_pin_on_loaded_token(&self) {
        let loaded = self.inner.read().clone();
        if let Some(auth) = loaded
            && let Some(e) = self.cached_token_policy_error(&auth)
        {
            self.reject_and_clear(&e);
        }
    }

    /// Override the proxy base URL (precedence over env var).
    pub(crate) fn with_proxy_base_url(mut self, url: &str) -> Self {
        self.proxy_base_url = url.to_owned();
        self
    }

    // ── State mutation (clear, hot_swap, update) ──────────────────────

    pub(crate) fn clear(&self) -> std::io::Result<()> {
        self.remove_scope(&self.scope)
    }

    /// Remove a scope entry from auth.json. When `scope == self.scope`, also
    /// drops in-memory auth so a later `auth()` reports `NotLoggedIn`, not stale
    /// `invalid_grant` (the scoped verdict reads inert with no credential).
    /// Empties auth.json by deleting the file.
    ///
    /// Best-effort: takes a non-blocking lock and skips the disk write if
    /// another process holds it (the stale entry is cleaned up on next launch).
    pub(crate) fn remove_scope(&self, scope: &str) -> std::io::Result<()> {
        self.remove_scope_impl(scope)
    }

    fn remove_scope_impl(&self, scope: &str) -> std::io::Result<()> {
        let disk_mutation = if let Some(_lock) = lock::try_lock_auth_file_nonblocking(&self.path) {
            self.write_scope_removal(scope)? // lock released on drop
        } else {
            ScopeRemoval::SkippedLockUnavailable
        };
        // Intentional removal must be attributable from unified.jsonl:
        // downstream, a deliberately deleted auth.json is indistinguishable
        // from accidental loss (corruption, external deletion).
        echo_agent_telemetry::unified_log::warn(
            "auth: scope removed from auth.json",
            None,
            Some(serde_json::json!({
                "scope": scope,
                "is_current_scope": scope == self.scope,
                "disk_mutation": disk_mutation.label(),
                "path": self.path.display().to_string(),
            })),
        );
        if scope == self.scope {
            self.clear_inner();
            // Intentional logout/scope removal: drop sticky permanent so the
            // next state is NotLoggedIn, not a retained invalid_grant verdict.
            *self.permanent_failure.write() = None;
        }
        Ok(())
    }

    /// Drop `scope` from auth.json and persist, deleting the file when the last
    /// scope is gone. Caller holds the `auth.json` lock (taken by
    /// [`Self::remove_scope_impl`]).
    fn write_scope_removal(&self, scope: &str) -> std::io::Result<ScopeRemoval> {
        let Ok(mut auth_store) = read_auth_json(&self.path) else {
            return Ok(ScopeRemoval::SkippedUnreadable);
        };
        auth_store.remove(scope);
        if auth_store.is_empty() {
            let _ = std::fs::remove_file(&self.path);
            Ok(ScopeRemoval::FileDeleted)
        } else {
            write_auth_json(&self.path, &auth_store)?;
            Ok(ScopeRemoval::EntryRemoved)
        }
    }

    /// Drop the in-memory auth. Sticky `RefreshTokenRejected` still
    /// short-circuits with no live credential until a wire-valid login;
    /// non-sticky verdicts read absent once their scoped key is gone.
    fn clear_inner(&self) {
        *self.inner.write() = None;
    }

    /// Re-read `auth.json` and reconcile the in-memory cache with it.
    ///
    /// A disk read returning "no usable token" has very different meanings that
    /// must not be conflated:
    ///
    /// * [`DiskAuthState::EntryMissing`] — the file is readable but our scope is
    ///   gone. This is the trustworthy "logged out / scope removed" signal, so
    ///   the in-memory credentials (and any cached permanent_failure) are
    ///   dropped together.
    /// * [`DiskAuthState::FileMissing`] / [`DiskAuthState::Unreadable`] — a
    ///   *disk anomaly*. The classic case is the first read after wake-from-
    ///   sleep transiently resolving `auth.json` to `ENOENT`. This is **not**
    ///   proof the credentials are gone, so we retry briefly and — if it
    ///   persists — retain a still-live in-memory refresh token rather than
    ///   discard the only copy. The server (a 401 driving `permanent_failure`)
    ///   stays the authority on whether the token is actually dead.
    pub(crate) fn force_reload_from_disk(&self) {
        self.force_reload_from_disk_with(RELOAD_RETRY_TRIES, RELOAD_RETRY_BACKOFF);
    }

    /// Inner of [`force_reload_from_disk`] with the retry budget injectable so
    /// the disk-anomaly branch is unit-testable without real sleeps.
    fn force_reload_from_disk_with(&self, tries: usize, backoff: StdDuration) {
        let mut last_state = DiskAuthState::FileMissing;
        for attempt in 0..tries.max(1) {
            if attempt > 0 && !backoff.is_zero() {
                std::thread::sleep(backoff);
            }
            let (auth, state) = self.read_disk_auth_with_state();
            last_state = state;
            match state {
                // Healthy entry on disk: regular swap. Do NOT clear the
                // permanent_failure here -- the token may just be a re-export
                // of the same broken refresh_token.
                DiskAuthState::Ok => {
                    *self.inner.write() = auth;
                    // A re-read (e.g. relay reconnect) can adopt a wrong-team
                    // token a sibling wrote; clear it here, mirroring `new()`.
                    self.enforce_pin_on_loaded_token();
                    return;
                }
                // File readable, our scope genuinely absent: the trustworthy
                // logout / scope-removed signal.
                DiskAuthState::EntryMissing => {
                    self.drop_in_memory_credentials("scope absent on readable auth.json");
                    self.enforce_pin_on_loaded_token();
                    return;
                }
                // Disk anomaly: a transient (e.g. wake-time ENOENT) heals on a
                // retry; a real loss persists across the budget.
                DiskAuthState::FileMissing | DiskAuthState::Unreadable => {}
            }
        }

        // Persistent disk anomaly. Discarding a live refresh token here is the
        // step that turns a transient disk blip into irreversible credential
        // loss (the RT may exist nowhere else), so retain it unless it is
        // already known-dead or there is nothing to protect. "Known-dead"
        // means a STICKY verdict (`RefreshTokenRejected` — the IdP rejected
        // this very credential): a recoverable verdict (`ClientRejected` /
        // escalated `Other`, e.g. three post-wake network blips) says nothing
        // about the RT's validity, and a wake-time FS anomaly co-occurring
        // with those blips must not forfeit the only copy of a live RT.
        let in_mem = self.current_or_expired();
        let sticky_verdict = matches!(
            self.permanent_failure(),
            Some(AuthError::Refresh(crate::auth::error::RefreshTokenError::Permanent(ref e)))
                if e.reason.is_sticky()
        );
        let retain = in_mem.as_ref().is_some_and(|a| a.refresh_token.is_some()) && !sticky_verdict;
        if let Some(a) = in_mem.filter(|_| retain) {
            echo_agent_telemetry::unified_log::warn(
                "auth: disk anomaly, retaining in-memory credentials",
                None,
                Some(serde_json::json!({
                    "disk_state": format!("{last_state:?}"),
                    "retained_key_prefix": bearer_suffix(&a.key),
                    "was_expired": is_expired(&a),
                })),
            );
            // In-memory credentials kept as-is.
        } else {
            self.drop_in_memory_credentials(
                "disk anomaly; no live refresh token to retain (missing RT or permanent failure)",
            );
        }
        self.enforce_pin_on_loaded_token();
    }

    /// Drop the in-memory credentials, loudly. Logs the discard (with `reason`)
    /// before routing through [`clear_inner`]. Also clears a sticky permanent
    /// verdict so force-reload / disk-anomaly paths surface `NotLoggedIn`
    /// rather than a retained `invalid_grant`. Permanent discard after a live
    /// IdP rejection uses [`clear_inner`] alone so the sticky short-circuit
    /// survives until login.
    fn drop_in_memory_credentials(&self, reason: &str) {
        if let Some(d) = self.current_or_expired() {
            echo_agent_telemetry::unified_log::warn(
                "auth: in-memory credentials dropped (disk reload found none)",
                None,
                Some(serde_json::json!({
                    "reason": reason,
                    "dropped_key_prefix": bearer_suffix(&d.key),
                    "had_refresh_token": d.refresh_token.is_some(),
                    "was_expired": is_expired(&d),
                    "disk_state": (*self.disk_state.read()).map(|s| format!("{s:?}")),
                })),
            );
        }
        self.clear_inner();
        *self.permanent_failure.write() = None;
    }

    // ── Read methods ─────────────────────────────────────────────────
    //
    // | wire-bound bearer            | `auth().await` / `get_valid_token().await` |
    // | cached, no refresh           | `current()` (5-min buffer) |
    // | any in-memory bearer         | `current_or_expired()` |
    // | expired entry (for its RT)   | `expired_auth()` |
    // | "have credentials at all?"   | `is_expired()` |
    // | bypass memory, read disk     | `read_disk_auth()` |

    // ── Login-policy enforcement (force_login_team_uuid) ──────────────
    //
    // The team pin is enforced wherever the manager hands out a session token,
    // not only on fresh login: sync reads (`current`/`expired_auth`) hide a
    // violating token; the async gates (`auth`, recovery) and `new()` also
    // clear `auth.json` to force a compliant re-login.

    /// `Some(error)` when a `force_login_team_uuid` pin is set and the token's
    /// team principal isn't allowed; `None` when compliant or unpinned.
    ///
    /// Reads the principal from the token's own (unverified) JWT claim —
    /// fail-fast defense-in-depth, not the security boundary (the server is
    /// authoritative). An API-key session is rejected under the kill switch,
    /// else allowed.
    pub(crate) fn cached_token_policy_error(&self, auth: &EchoAgentAuth) -> Option<AuthError> {
        if auth.auth_mode == AuthMode::ApiKey {
            // Else enforce_disable_api_key_auth swaps the key for itself (no-op).
            return self
                .echo_agent_com_config
                .api_key_auth_disabled()
                .then_some(AuthError::ApiKeyAuthDisabled);
        }
        let policy = crate::auth::oidc::login_principal_policy(&self.echo_agent_com_config)?;
        let actual = crate::auth::oidc::peek_access_token_principal_id(&auth.key);
        crate::auth::oidc::enforce_login_principal(Some(&policy), actual.as_deref())
            .err()
            .map(|e| AuthError::PinnedTeamMismatch {
                message: e.to_string(),
            })
    }

    /// Log and clear a policy-violating session (disk + memory) so the next
    /// launch forces a fresh, compliant login.
    pub(crate) fn reject_and_clear(&self, error: &AuthError) {
        let policy = match error {
            AuthError::PinnedTeamMismatch { .. } => "team_pin",
            AuthError::ApiKeyAuthDisabled => "api_key_disabled",
            _ => "login_policy",
        };
        echo_agent_telemetry::unified_log::warn(
            "auth: cached session rejected by login policy; clearing",
            None,
            Some(serde_json::json!({ "policy": policy, "reason": error.to_string() })),
        );
        if let Err(e) = self.clear() {
            tracing::warn!(error = %e, "auth: failed to clear policy-violating session");
        }
    }

    /// Hide a cached token rejected by the login policy. No clear here (keeps
    /// the sync read path lock-free); `auth()`/recovery/`new()` do the clearing.
    fn vet_cached(&self, auth: EchoAgentAuth) -> Option<EchoAgentAuth> {
        match self.cached_token_policy_error(&auth) {
            None => Some(auth),
            Some(e) => {
                tracing::debug!(error = %e, "auth: hiding cached session rejected by login policy");
                None
            }
        }
    }

    /// Cached in-memory token if outside the early-invalidation buffer.
    pub(crate) fn current(&self) -> Option<EchoAgentAuth> {
        let auth = self
            .inner
            .read()
            .as_ref()
            .filter(|a| !self.is_token_expired(a))
            .cloned()?;
        self.vet_cached(auth)
    }

    /// Closure-scoped write. Sync return type prevents `.await` while
    /// the lock is held. Prefer this over `self.inner.write()`.
    #[inline]
    pub(crate) fn with_inner_write<R>(&self, f: impl FnOnce(&mut Option<EchoAgentAuth>) -> R) -> R {
        let mut guard = self.inner.write();
        f(&mut guard)
    }

    /// Closure-scoped read counterpart to [`Self::with_inner_write`].
    #[inline]
    pub(crate) fn with_inner_read<R>(&self, f: impl FnOnce(Option<&EchoAgentAuth>) -> R) -> R {
        let guard = self.inner.read();
        f(guard.as_ref())
    }

    /// Returns true if credentials exist but have expired.
    pub(crate) fn is_expired(&self) -> bool {
        self.inner
            .read()
            .as_ref()
            .is_some_and(|a| self.is_token_expired(a))
    }

    /// In-memory bearer regardless of the early-invalidation buffer.
    /// Prefer [`Self::auth`] when `.await` is available.
    pub(crate) fn current_or_expired(&self) -> Option<EchoAgentAuth> {
        self.current().or_else(|| self.expired_auth())
    }

    /// Cached token if still wire-valid ([`Self::is_token_hard_expired`]),
    /// ignoring the early-invalidation buffer. For sync callers that cannot
    /// refresh and must not demote a still-accepted token.
    pub(crate) fn current_wire_valid(&self) -> Option<EchoAgentAuth> {
        let auth = self
            .inner
            .read()
            .as_ref()
            .filter(|a| !self.is_token_hard_expired(a))
            .cloned()?;
        self.vet_cached(auth)
    }

    /// `true` when data collection must be suppressed — the team has ZDR or
    /// the user opted out of coding data retention. Reads
    /// [`Self::current_or_expired`] because neither flag changes on token
    /// expiry and `current()` returns `None` during the refresh window.
    ///
    /// Fail-open: no credential ⇒ `false` (not disabled). Collection paths
    /// that must not act on unknown privacy state should use the fail-closed
    /// [`Self::allows_data_collection`] instead.
    pub(crate) fn is_data_collection_disabled(&self) -> bool {
        self.current_or_expired()
            .is_some_and(|a| a.is_data_collection_disabled())
    }

    /// Fail-closed collection predicate: `true` only when a credential
    /// exists and carries no ZDR / retention-opt-out flag. Missing or
    /// cleared auth (e.g. after a mid-session `/logout`) counts as
    /// disabled — nothing may leave the machine while the privacy state is
    /// unknown.
    pub(crate) fn allows_data_collection(&self) -> bool {
        self.current_or_expired()
            .is_some_and(|a| !a.is_data_collection_disabled())
    }

    /// Expired in-memory entry (for its `refresh_token`).
    pub(crate) fn expired_auth(&self) -> Option<EchoAgentAuth> {
        let auth = self
            .inner
            .read()
            .as_ref()
            .filter(|a| self.is_token_expired(a))
            .cloned()?;
        self.vet_cached(auth)
    }

    /// Expiry policy: `expires_at - early_invalidation` if present;
    /// `External` with `auth_token_ttl` -> `create_time + ttl`;
    /// fallback `create_time + 30d` (WebLogin-style).
    fn is_token_expired(&self, auth: &EchoAgentAuth) -> bool {
        self.token_expired_with_buffer(auth, early_invalidation())
    }

    /// Actual (hard) expiry: the instant the proxy would actually reject the
    /// token, with no early-invalidation margin. The export gate
    /// ([`Self::has_usable_token`]) uses this instead of [`Self::is_token_expired`]
    /// because a token still inside the buffer is sent — and accepted — on the
    /// wire via `current_or_expired()`, so it must not count as unusable.
    fn is_token_hard_expired(&self, auth: &EchoAgentAuth) -> bool {
        self.token_expired_with_buffer(auth, Duration::zero())
    }

    fn token_expired_with_buffer(&self, auth: &EchoAgentAuth, buffer: Duration) -> bool {
        if auth.expires_at.is_some() {
            return is_expired_with_buffer(auth, buffer);
        }
        if auth.auth_mode == AuthMode::External
            && let Some(ttl) = self.echo_agent_com_config.auth_token_ttl
        {
            let age = Utc::now().signed_duration_since(auth.create_time);
            return age >= Duration::seconds(ttl as i64) - buffer;
        }
        is_expired_with_buffer(auth, buffer)
    }

    // ── Persistence + enrichment ──────────────────────────────────────

    /// Persist rotated tokens to disk + cache, then spawn `/user` enrichment.
    ///
    /// Invariants:
    /// - **Disk write before any network I/O** (else a sibling process can
    ///   reuse the not-yet-rotated RT and the IdP returns `invalid_grant`).
    /// - **Caller holds the `auth.json` file lock** (production callers:
    ///   `refresh_chain` Success arm, `flow::run_auth_flow`).
    ///
    /// Returns the input `EchoAgentAuth` BEFORE enrichment lands; callers
    /// needing the post-enrichment view re-read `current()`.
    pub(crate) async fn update(
        self: &Arc<Self>,
        auth: EchoAgentAuth,
    ) -> std::io::Result<EchoAgentAuth> {
        let update_started = std::time::Instant::now();
        let map = match read_auth_json_or_empty_recovering_corrupt(&self.path) {
            Ok(map) => map,
            Err(e) => {
                // Non-recoverable error (PermissionDenied, etc.) — keep conservative.
                tracing::warn!(error = %e, "auth: read failed, updating in-memory only");
                echo_agent_telemetry::unified_log::warn(
                    "auth update skipped disk write (read failed)",
                    None,
                    Some(serde_json::json!({ "error": e.to_string() })),
                );
                self.with_inner_write(|inner| *inner = Some(auth.clone()));
                self.spawn_user_info_enrichment(auth.clone());
                return Ok(auth);
            }
        };
        let mut map = map;
        // One entry per scope (personal and team share the scope key).
        tracing::debug!(scope = %self.scope, "auth: storing token");
        map.insert(self.scope.clone(), auth.clone());
        let write_result = write_auth_json(&self.path, &map);
        let elapsed_ms = update_started.elapsed().as_millis() as u64;
        match &write_result {
            Ok(()) => echo_agent_telemetry::unified_log::info(
                "auth update disk written",
                None,
                Some(serde_json::json!({
                    "rt_prefix": auth.refresh_token.as_deref().map(bearer_suffix),
                    "key_prefix": bearer_suffix(&auth.key),
                    "elapsed_ms": elapsed_ms,
                })),
            ),
            Err(e) => echo_agent_telemetry::unified_log::error(
                "auth update disk write failed",
                None,
                Some(serde_json::json!({
                    "error": e.to_string(),
                    "elapsed_ms": elapsed_ms,
                })),
            ),
        }
        // Always update in-memory, even if disk write failed. This lets the
        // current session work with fresh credentials while the user fixes the
        // filesystem (e.g. read-only disk). Without this, a disk failure leaves
        // the stale/dead token in memory and the user is completely stuck.
        *self.permanent_failure.write() = None;
        self.with_inner_write(|inner| *inner = Some(auth.clone()));

        // Fire-and-forget enrichment. Off the critical path -- a slow
        // `/user` would otherwise widen the sibling-process
        // `invalid_grant` race window.
        self.spawn_user_info_enrichment(auth.clone());

        write_result?;
        Ok(auth)
    }

    /// Persist to disk and cache without spawning the background `/user` task
    /// (already merged inline, or a stale fetch must not race a fresh write).
    pub(crate) async fn save_without_enrichment(
        &self,
        auth: EchoAgentAuth,
    ) -> std::io::Result<EchoAgentAuth> {
        let started = std::time::Instant::now();
        let map = match read_auth_json_or_empty_recovering_corrupt(&self.path) {
            Ok(map) => map,
            Err(e) => {
                // Non-recoverable error — keep conservative.
                tracing::warn!(error = %e, "auth: read failed, updating in-memory only (no enrichment)");
                echo_agent_telemetry::unified_log::warn(
                    "auth update skipped disk write (read failed, no enrichment)",
                    None,
                    Some(serde_json::json!({ "error": e.to_string() })),
                );
                self.with_inner_write(|inner| *inner = Some(auth.clone()));
                return Ok(auth);
            }
        };
        let mut map = map;
        tracing::debug!(scope = %self.scope, "auth: storing token (no enrichment)");
        map.insert(self.scope.clone(), auth.clone());
        let write_result = write_auth_json(&self.path, &map);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        match &write_result {
            Ok(()) => echo_agent_telemetry::unified_log::info(
                "auth update disk written (no enrichment)",
                None,
                Some(serde_json::json!({
                    "rt_prefix": auth.refresh_token.as_deref().map(bearer_suffix),
                    "key_prefix": bearer_suffix(&auth.key),
                    "elapsed_ms": elapsed_ms,
                })),
            ),
            Err(e) => echo_agent_telemetry::unified_log::error(
                "auth update disk write failed (no enrichment)",
                None,
                Some(serde_json::json!({
                    "error": e.to_string(),
                    "elapsed_ms": elapsed_ms,
                })),
            ),
        }
        // Always update in-memory, even if disk write failed (see update()).
        *self.permanent_failure.write() = None;
        self.with_inner_write(|inner| *inner = Some(auth.clone()));
        write_result?;
        Ok(auth)
    }

    /// Spawn the `/user` enrichment task; body in the `enrichment` submodule.
    fn spawn_user_info_enrichment(self: &Arc<Self>, auth: EchoAgentAuth) {
        enrichment::spawn(Arc::clone(self), auth);
    }

    /// Blocking `/user` enrichment for login flows that exit before the background task lands.
    pub(crate) async fn enrich_auth_inline(&self, auth: &mut EchoAgentAuth) {
        enrichment::enrich_inline(self, auth).await;
    }

    /// Path to the `auth.json` this manager reads/writes (respects
    /// `ECHO_AGENT_AUTH_PATH` / constructor home). Prefer this over
    /// `echo_agent_home()/auth.json` so temp-home tests and custom stores stay isolated.
    pub(crate) fn auth_json_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn echo_agent_com_config(&self) -> &EchoAgentComConfig {
        &self.echo_agent_com_config
    }

    /// Handle notified after every successful token refresh.
    ///
    /// Used by [`ModelsManager`] to trigger model catalog recovery
    /// after sleep/wake, bypassing the FSEvents file watcher which
    /// can silently die on macOS after resume.
    pub(crate) fn refresh_notifier(&self) -> Arc<tokio::sync::Notify> {
        self.refresh_notify.clone()
    }

    /// Wake the proactive-refresh loop out of its (monotonic) timer. Called by
    /// the power listener on every `DidWake` (see
    /// [`Self::set_system_sleep_imminent`]); safe from any thread —
    /// `Notify::notify_waiters` is sync and runtime-agnostic.
    pub(crate) fn notify_wake(&self) {
        self.wake_notify.notify_waiters();
    }

    /// Wait up to `timeout` for another consumer (proactive refresh task,
    /// main request path) to refresh the token.  Returns `true` if the
    /// in-memory token changed during the wait.
    ///
    /// Background consumers (signals sync, turn deltas) use this to defer
    /// to the primary refresh path instead of driving their own
    /// `ServerRejected` recovery, avoiding concurrent refresh storms that
    /// amplify 401 bursts at CCP.
    pub(crate) async fn wait_for_token_refresh(&self, timeout: std::time::Duration) -> bool {
        let pre_key = self.current().map(|a| a.key.clone());
        tokio::select! {
            _ = self.refresh_notify.notified() => {}
            _ = tokio::time::sleep(timeout) => {}
        }
        let post_key = self.current().map(|a| a.key.clone());
        post_key != pre_key
    }

    /// Run the external auth command and parse its output. Pure: no
    /// state mutation, no logging (refresher logs once on its arm).
    pub(crate) async fn run_external_refresh_command(
        &self,
        command: &str,
    ) -> Option<EchoAgentAuth> {
        let prev = self.inner_auth_or_external_default();
        crate::auth::refresh_with_command(command, &prev).await
    }

    /// Hot-swap credentials (called by config watcher). Does NOT write to disk.
    /// Clears a sticky permanent verdict only when the new bearer is wire-valid
    /// (login / sibling adopt). Hard-expired swaps keep the sticky short-circuit
    /// so a dead RT is not re-tried until a real login.
    pub(crate) fn hot_swap(&self, new_auth: EchoAgentAuth) {
        if !self.is_token_hard_expired(&new_auth) {
            *self.permanent_failure.write() = None;
        }
        self.with_inner_write(|inner| *inner = Some(new_auth));
    }

    /// Clear in-memory credentials. Does NOT touch disk. Sticky
    /// `RefreshTokenRejected` remains until wire-valid login; other verdicts
    /// are key-scoped and drop out once their credential is gone.
    pub(crate) fn clear_in_memory(&self) {
        self.clear_inner();
    }

    // ── Disk I/O helpers ──────────────────────────────────────────────

    /// Accept a sibling-rotated disk token. On `ServerRejected`, the
    /// disk key must differ from in-memory (else no one refreshed).
    ///
    /// Single enforcement point for disk adoption: `try_adopt_disk_token`
    /// (refresh chains) and `pick_up_sibling_token` (`auth()` / proactive
    /// loop) both route here, so the guards and the shared `hot_swap`
    /// cannot drift between the two paths.
    pub(crate) fn try_use_disk_token(
        &self,
        disk_auth: Option<&EchoAgentAuth>,
        reason: RefreshReason,
    ) -> Result<EchoAgentAuth, DiskTokenDecline> {
        let Some(disk_auth) = disk_auth else {
            return Err(DiskTokenDecline::Missing);
        };
        if self.is_token_expired(disk_auth) {
            return Err(DiskTokenDecline::Expired);
        }
        // A disk token minted before the live in-memory one is not a sibling
        // rotation — it's disk lagging memory (`update()` keeps a successful
        // mint in memory when its disk write fails). Adopting it would clobber
        // the fresher credential; on `ServerRejected` it would restore the very
        // bearer the caller is rejecting. Skew tolerance: on a shared/networked
        // auth.json a sibling machine's clock can stamp a genuinely newer
        // rotation slightly older, so fail toward adoption within the window —
        // a wrong adopt self-corrects via 401 -> ServerRejected, a wrong mint
        // burns the refresh-token family (60s matches PROVIDER_TOKEN_EXPIRY_SKEW).
        const DISK_MINT_SKEW_TOLERANCE: Duration = Duration::seconds(60);
        // `current_or_expired()`, not `current()`: adoption runs exactly when
        // the live bearer needs a refresh — canonically inside the five-minute
        // early-invalidation buffer, which `current()` hides. Reading through
        // `current()` skipped this guard in precisely the window that routes
        // callers here; a buffered bearer is still the newest local mint and
        // must not be clobbered by a lagging disk token.
        if let Some(current) = self.current_or_expired()
            && disk_auth.create_time + DISK_MINT_SKEW_TOLERANCE < current.create_time
        {
            return Err(DiskTokenDecline::LaggingMemoryMint);
        }
        if reason == RefreshReason::ServerRejected {
            let current_key = self.inner.read().as_ref().map(|a| a.key.clone());
            if current_key.as_deref() == Some(&disk_auth.key) {
                return Err(DiskTokenDecline::SameKeyAsRejected);
            }
        }
        tracing::info!("auth: another process already refreshed, using disk token");
        self.hot_swap(disk_auth.clone());
        Ok(disk_auth.clone())
    }

    /// Re-read disk and try to adopt a sibling-written token, emitting
    /// telemetry on success. Combines `read_disk_auth` +
    /// `try_use_disk_token` + the structured log that was previously
    /// duplicated at each callsite in `refresh_chain`.
    fn try_adopt_disk_token(&self, reason: RefreshReason, msg: &str) -> Option<EchoAgentAuth> {
        let disk_auth = self.read_disk_auth();
        // Snapshot the pre-adoption key BEFORE `try_use_disk_token` hot-swaps
        // the in-memory bearer: reading it afterwards always yielded `None` /
        // `key_changed: true`, corrupting the prev/adopted attribution this
        // log exists to capture.
        let prev = self
            .current_or_expired()
            .map(|a| bearer_suffix(&a.key).to_owned());
        let refreshed = match self.try_use_disk_token(disk_auth.as_ref(), reason) {
            Ok(refreshed) => refreshed,
            // `Missing` / `Expired` are the steady state at every
            // refresh-chain callsite (usually there is no sibling token to
            // adopt); only the two guard declines are decisions worth a
            // structured-log line when reconstructing a rotation chain.
            Err(
                decline @ (DiskTokenDecline::LaggingMemoryMint
                | DiskTokenDecline::SameKeyAsRejected),
            ) => {
                echo_agent_telemetry::unified_log::info(
                    "auth: disk token declined",
                    None,
                    Some(serde_json::json!({
                        "decline": decline.as_str(),
                        "refresh_reason": format!("{reason:?}"),
                        "prev_key_prefix": prev,
                        "disk_key_prefix": disk_auth.as_ref().map(|a| bearer_suffix(&a.key)),
                    })),
                );
                return None;
            }
            Err(_) => return None,
        };
        let adopted = bearer_suffix(&refreshed.key);
        echo_agent_telemetry::unified_log::info(
            msg,
            None,
            Some(serde_json::json!({
                "adopted_key_prefix": adopted,
                "prev_key_prefix": prev,
                "key_changed": prev.as_deref() != Some(adopted),
            })),
        );
        Some(refreshed)
    }

    /// Current auth or an `External`-defaulted placeholder. **External
    /// path only** -- the placeholder's `auth_mode = External` would
    /// mis-classify an OIDC token. Carries user fields forward into the
    /// binary's freshly-minted token.
    fn inner_auth_or_external_default(&self) -> EchoAgentAuth {
        self.inner.read().clone().unwrap_or_else(|| EchoAgentAuth {
            auth_mode: AuthMode::External,
            ..Default::default()
        })
    }

    /// Test-only hot_swap + disk write (skips proxy `/user`).
    /// Production persistence routes through `update()`.
    #[cfg(test)]
    fn persist_and_swap(&self, auth: EchoAgentAuth) -> Option<EchoAgentAuth> {
        self.hot_swap(auth.clone());
        let mut map = match read_auth_json_or_empty(&self.path) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "auth: read failed in persist_and_swap, skipping disk write");
                return Some(auth);
            }
        };
        map.insert(self.scope.clone(), auth.clone());
        if let Err(e) = write_auth_json(&self.path, &map) {
            tracing::warn!(error = %e, "auth: failed to persist refreshed token to disk");
        }
        Some(auth)
    }

    /// `true` when the refresh token on disk is present and differs from the
    /// one we actually spent — i.e. a sibling process rotated the RT while our
    /// exchange was in flight, so the rejection we just got is a lost race
    /// rather than a revoked session.
    ///
    /// The single definition of "disk moved past the token we spent". Two
    /// hand-rolled copies of this comparison is how the wrong one survived
    /// long enough to log a dozen processes out at once.
    ///
    /// Takes an already-observed `disk_rt` rather than reading `auth.json`
    /// itself, so one observation drives the decision, the unattributed
    /// fallback, and the telemetry that explains them. A second read can catch
    /// a *later* sibling write and produce a record that contradicts the
    /// branch it documents — in the log whose whole purpose is post-incident
    /// truth. Callers read under the auth file lock, so the observation
    /// includes the sibling's committed write.
    ///
    /// Disk holding no RT is *not* divergence: there is no successor to fall
    /// back to, so the rejection must be honored.
    fn refresh_token_superseded(disk_rt: Option<&str>, spent_rt: &str) -> bool {
        disk_rt.is_some_and(|disk_rt| disk_rt != spent_rt)
    }

    /// `true` when a sibling process has rotated the refresh token on disk
    /// past the one in memory. Used by `refresh_chain` to demote a
    /// `PermanentFailure` to transient so the sibling's fresher token can be
    /// tried on the next attempt.
    ///
    /// Requires an in-memory RT: empty `inner` means the disk credential is
    /// the only candidate (not a multi-process rotation). Does **not**
    /// require a non-expired disk AT — a sibling may still hold a usable RT
    /// while its AT is buffer/hard-expired.
    ///
    /// Only a fallback for authorities that cannot report which RT they spent.
    /// Attributed refreshers pass the token they actually sent to
    /// [`Self::refresh_token_superseded`] directly; because
    /// `resolve_refresh_credential` is disk-first, the RT actually sent is
    /// usually the disk one, and comparing disk against *memory* then answers
    /// `false` in precisely the case that needs the demotion.
    fn sibling_has_different_refresh_token(&self, disk_rt: Option<&str>) -> bool {
        self.current_or_expired()
            .and_then(|a| a.refresh_token)
            .is_some_and(|mem_rt| Self::refresh_token_superseded(disk_rt, &mem_rt))
    }

    /// Re-read `auth.json` from disk without updating in-memory state.
    pub(crate) fn read_disk_auth(&self) -> Option<EchoAgentAuth> {
        self.read_disk_auth_with_state().0
    }

    /// Disk read for the configured scope with NO observation side effects (no
    /// `disk_state` write, no transition telemetry). For side-effect-free
    /// getters like [`Self::attempted_verdict_key`]; prefer [`Self::read_disk_auth`]
    /// when the read should drive transition logging.
    fn read_disk_auth_silent(&self) -> Option<EchoAgentAuth> {
        read_auth_json(&self.path)
            .ok()
            .and_then(|map| lookup_auth(&map, &self.scope))
    }

    /// Wire-valid token present in on-disk `auth.json`, judged by actual expiry
    /// ([`Self::is_token_hard_expired`]); never mutates in-memory state, unlike
    /// [`Self::force_reload_from_disk`].
    pub(crate) fn has_usable_disk_token(&self) -> bool {
        self.read_disk_auth()
            .is_some_and(|a| !self.is_token_hard_expired(&a))
    }

    /// Whether a wire-valid token is available in memory or on disk — a
    /// credential worth a real outbound attempt. Judged by actual expiry so it
    /// mirrors the `current_or_expired()` bearer the senders put on the wire; a
    /// token inside the early-invalidation buffer still counts.
    pub(crate) fn has_usable_token(&self) -> bool {
        self.current_or_expired()
            .is_some_and(|a| !self.is_token_hard_expired(&a))
            || self.has_usable_disk_token()
    }

    /// Like [`read_disk_auth`] but also returns the [`DiskAuthState`] so callers
    /// can tell a transient disk anomaly (`FileMissing`/`Unreadable`) apart from
    /// a genuine logout (`EntryMissing`). Observes the state for transition
    /// logging, exactly like `read_disk_auth`.
    pub(crate) fn read_disk_auth_with_state(&self) -> (Option<EchoAgentAuth>, DiskAuthState) {
        let (auth, state, err_detail) = match read_auth_json(&self.path) {
            Ok(map) => {
                let found = lookup_auth(&map, &self.scope);
                let state = if found.is_some() {
                    DiskAuthState::Ok
                } else {
                    DiskAuthState::EntryMissing
                };
                (found, state, None)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                (None, DiskAuthState::FileMissing, None)
            }
            Err(e) => {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %e,
                    "auth: failed to read auth.json"
                );
                (None, DiskAuthState::Unreadable, Some(e.to_string()))
            }
        };
        self.observe_disk_state(state, auth.as_ref(), err_detail);
        (auth, state)
    }

    /// Transition-level unified logging for the on-disk auth state:
    /// exactly one line per state change. Hot retry loops must produce
    /// neither a log flood nor silence — a single attributable event at
    /// the moment auth.json disappears (and one when it returns).
    fn observe_disk_state(
        &self,
        new_state: DiskAuthState,
        auth: Option<&EchoAgentAuth>,
        err_detail: Option<String>,
    ) {
        let prev = {
            let mut guard = self.disk_state.write();
            let prev = *guard;
            *guard = Some(new_state);
            prev
        };
        if prev == Some(new_state) {
            return;
        }
        let ctx = serde_json::json!({
            "from": prev.map(|s| format!("{s:?}")),
            "to": format!("{new_state:?}"),
            "path": self.path.display().to_string(),
            "scope": &self.scope,
            "error": err_detail,
            "key_prefix": auth.map(|a| bearer_suffix(&a.key).to_owned()),
            "has_refresh_token": auth.map(|a| a.refresh_token.is_some()),
            "is_expired": auth.map(is_expired),
        });
        match new_state {
            // Recovery (or first observation in ECHO_AGENT_AUTH mode).
            DiskAuthState::Ok => {
                echo_agent_telemetry::unified_log::info(
                    "auth disk state: entry present",
                    None,
                    Some(ctx),
                );
            }
            // Credential loss on disk — the line that answers "when did
            // auth.json disappear and what did this process see".
            DiskAuthState::FileMissing
            | DiskAuthState::EntryMissing
            | DiskAuthState::Unreadable => {
                echo_agent_telemetry::unified_log::warn(
                    "auth disk state: entry lost",
                    None,
                    Some(ctx),
                );
            }
        }
    }

    pub(crate) async fn try_lock_auth_file_async(
        &self,
        timeout: StdDuration,
        heartbeat: lock::Heartbeat,
    ) -> LockAcquire {
        try_lock_auth_file_async(&self.path, timeout, heartbeat).await
    }

    // ── Refresher setup ─────────────────────────────────────────────

    /// Set up refresh capability. Call once per `Arc<AuthManager>` at
    /// startup; subsequent calls are no-op via an atomic guard (so
    /// per-session call sites don't reset refresher-internal state
    /// like `OidcRefresher::upload_in_flight`). Returns `true` if
    /// this call installed the refresher.
    pub fn configure_refresher(
        self: &Arc<Self>,
        auth_provider_command: Option<String>,
        diagnostic_uploader: Option<super::refresh::DiagnosticUploader>,
    ) -> bool {
        use std::sync::atomic::Ordering;
        // Idempotent: the AcqRel CAS publishes the subsequent
        // `refresher.write()` to any reader that observes
        // `refresher_configured == true`; the Acquire-failure pairs.
        if self
            .refresher_configured
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::debug!("auth: configure_refresher already wired; ignoring");
            return false;
        }
        let refresher = super::refresh::build_refresher(
            Arc::clone(self),
            auth_provider_command,
            diagnostic_uploader,
        );
        *self.refresher.write() = Some(refresher);
        true
    }

    /// Test-only: inject a refresher, bypassing the idempotency guard.
    #[cfg(test)]
    pub(crate) fn set_refresher(&self, refresher: Arc<dyn TokenRefresher>) {
        use std::sync::atomic::Ordering;
        *self.refresher.write() = Some(refresher);
        self.refresher_configured.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn proactive_iteration_count(&self) -> u32 {
        self.proactive_iter_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn proactive_start_count(&self) -> u32 {
        self.proactive_starts
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `pub(super)` — for refresh dispatch only. External session
    /// classification uses `is_session_based_method`.
    pub(super) fn token_type(&self) -> TokenType {
        TokenType::from_auth(self.inner.read().as_ref())
    }

    // ── Pre-request dispatch ──────────────────────────────────────────

    /// Pre-request entry point: per-`TokenType` dispatch. For just the key:
    /// [`Self::get_valid_token`].
    ///
    /// Also the team-pin gate: a cached/refreshed wrong-team session is cleared
    /// and rejected here, never handed to a consumer.
    #[tracing::instrument(skip(self), fields(token_type = tracing::field::Empty))]
    pub async fn auth(self: &Arc<Self>) -> Result<EchoAgentAuth, AuthError> {
        let auth = self.auth_dispatch().await?;
        if let Some(e) = self.cached_token_policy_error(&auth) {
            self.reject_and_clear(&e);
            return Err(e);
        }
        Ok(auth)
    }

    async fn auth_dispatch(self: &Arc<Self>) -> Result<EchoAgentAuth, AuthError> {
        // Snapshot inner ONCE for dispatch atomicity (closes a TOCTOU
        // where a concurrent `clear()` raced `token_type()` + `inner.read()`).
        let snapshot: Option<EchoAgentAuth> = self.with_inner_read(|inner| inner.cloned());
        // Kept alongside `snapshot`, which the grace arm below consumes: the
        // devbox arms still need to name the credential they gave up on.
        let snapshot_key: Option<String> = snapshot.as_ref().map(|a| a.key.clone());
        let token_type = TokenType::from_auth(snapshot.as_ref());
        tracing::Span::current().record("token_type", tracing::field::debug(token_type));

        // Fast path (before permanent_failure so a hot_swap after
        // re-login isn't blocked by a stale failure).
        if let Some(ref auth) = snapshot
            && !self.is_token_expired(auth)
        {
            return Ok(auth.clone());
        }

        if let Some(err) = self.permanent_failure() {
            // The verdict is about the *refresh* token; a cached access token
            // that is still wire-valid ([`Self::is_token_hard_expired`]) is
            // usable regardless (no IdP).
            if let Some(ref auth) = snapshot
                && !self.is_token_hard_expired(auth)
            {
                return Ok(auth.clone());
            }
            // A sibling process may have refreshed while we were in
            // PermanentFailure. Check disk before giving up.
            if let Some(refreshed) = self.try_adopt_disk_token(
                RefreshReason::PreRequest,
                "auth: adopted sibling token during PermanentFailure in auth()",
            ) {
                return Ok(refreshed);
            }
            // On devboxes, try minting fresh credentials before giving up.
            // preferred_method=api_key forbids automatic OIDC mint.
            if !self.echo_agent_com_config.blocks_automatic_oidc()
                && self.is_devbox_environment()
                && let Ok(auth) = self.try_devbox_recovery(snapshot_key.as_deref()).await
            {
                return Ok(auth);
            }
            return Err(err);
        }

        let result = match token_type {
            TokenType::None => Err(AuthError::NotLoggedIn),
            TokenType::ApiKey => {
                // The fast path above already returned for the valid case.
                // Reaching here means either the snapshot was empty (no
                // ApiKey loaded — surface NotLoggedIn) or the cached
                // api_key has aged past the 30-day TTL (surface
                // TokenExpiredNoRefresh so downstream consumers see
                // the same view as the UI's login screen, instead of
                // cloning the stale key and hitting 401 right after.
                if snapshot.is_some() {
                    Err(AuthError::TokenExpiredNoRefresh)
                } else {
                    Err(AuthError::NotLoggedIn)
                }
            }
            TokenType::LegacySession => {
                // Deliberate side effect: re-read auth.json under the
                // assumption that a sibling process (`echoagent login` from
                // another shell, the desktop app, etc.) may have refreshed
                // the on-disk credentials. `pick_up_sibling_token` only
                // mutates inner when the disk holds a *different valid*
                // token, so the common cache-hit case is a single read.
                // Documented at module level under "Lock ordering".
                self.pick_up_sibling_token();
                self.current().ok_or(AuthError::TokenExpiredNoRefresh)
            }
            TokenType::OidcSession | TokenType::ExternalBinary => {
                match self
                    .refresh_chain(token_type, RefreshReason::PreRequest)
                    .await
                {
                    Ok(auth) => Ok(auth),
                    Err(e) => {
                        // Grace for still wire-valid ATs on transient failures
                        // and retain-path permanents (ClientRejected / Other).
                        // RefreshTokenRejected discards AT+RT — never re-serve
                        // that snapshot even when it is only soft-expired.
                        let deny_grace = matches!(
                            &e,
                            AuthError::Refresh(crate::auth::RefreshTokenError::Permanent(pe))
                                if pe.reason
                                    == crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected
                        );
                        if !deny_grace
                            && let Some(auth) = snapshot
                            && !self.is_token_hard_expired(&auth)
                        {
                            tracing::debug!(
                                "auth: refresh failed but token still valid (grace), using cached"
                            );
                            Ok(auth)
                        } else {
                            Err(e)
                        }
                    }
                }
            }
        };

        // Devbox last-resort recovery: if all normal auth/refresh paths
        // failed and we're on a devbox, try minting fresh credentials via
        // the remote devbox login helper. Purges existing auth.json and writes only
        // the new OIDC entry so we start from a clean state.
        // preferred_method=api_key forbids automatic OIDC mint.
        if result.is_err()
            && !self.echo_agent_com_config.blocks_automatic_oidc()
            && self.is_devbox_environment()
            && let Ok(auth) = self.try_devbox_recovery(snapshot_key.as_deref()).await
        {
            return Ok(auth);
        }

        result
    }

    /// Whether we're running inside a devbox environment. Wraps the free function
    /// so tests can pin it per-instance (CI may run in a container where it is `true`).
    pub(crate) fn is_devbox_environment(&self) -> bool {
        #[cfg(test)]
        if let Some(forced) = *self.devbox_override.lock() {
            return forced;
        }
        crate::auth::devbox_login::is_devbox_environment()
    }

    /// Force [`Self::is_devbox_environment`] in tests.
    #[cfg(test)]
    pub(crate) fn set_devbox_env_for_test(&self, is_devbox: bool) {
        *self.devbox_override.lock() = Some(is_devbox);
    }

    /// Last-resort devbox auth recovery: purge existing auth.json entirely
    /// and mint fresh OIDC credentials via the remote devbox login helper.
    /// Only callable on devboxes (where the local service-account token is
    /// available).
    ///
    /// Fail-closed under `preferred_method=api_key` (no automatic OIDC mint),
    /// including direct callers such as sampler 401 recovery.
    ///
    /// `unusable` is the credential the caller has already established cannot
    /// work — the bearer the server rejected, or the snapshot that failed to
    /// refresh. It is what makes the wait-on-the-lock double-check below mean
    /// "somebody else fixed this" instead of "the dead token is still here".
    pub(crate) async fn try_devbox_recovery(
        self: &Arc<Self>,
        unusable: Option<&str>,
    ) -> Result<EchoAgentAuth, AuthError> {
        if self.echo_agent_com_config.blocks_automatic_oidc() {
            tracing::debug!(
                "auth: devbox recovery skipped (preferred_method=api_key blocks automatic OIDC)"
            );
            echo_agent_telemetry::unified_log::info(
                "auth: devbox recovery skipped (preferred_method=api_key)",
                None,
                None,
            );
            return Err(AuthError::NotLoggedIn);
        }

        let _guard = self.refresh_lock.lock().await;

        // Double-check: another task may have recovered while we waited. Only
        // a credential that is not the caller's `unusable` one counts. Without
        // that filter a 401 on a still-locally-valid bearer reports recovery
        // with the very token the server just rejected, and the caller
        // resubmits it until its retry budget runs out.
        if let Some(auth) = self
            .current()
            .filter(|auth| unusable != Some(auth.key.as_str()))
        {
            return Ok(auth);
        }

        tracing::info!("auth: attempting devbox recovery (purge + re-mint)");
        echo_agent_telemetry::unified_log::info("auth: devbox recovery starting", None, None);

        // Raw mint: the `/user` merge would block up to 10s under refresh_lock.
        let new_auth = super::devbox_login::mint_devbox_auth_raw()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "auth: devbox recovery mint failed");
                AuthError::transient_source(e)
            })?;

        // Purge auth.json so we start clean — removes any corrupted,
        // revoked, or legacy entries that caused the failure.
        let _ = tokio::fs::remove_file(&self.path).await;
        self.clear_inner();

        let auth = self.save_without_enrichment(new_auth).await.map_err(|e| {
            tracing::warn!(error = %e, "auth: devbox recovery save failed");
            AuthError::transient_source(e)
        })?;

        // ZDR flags arrive via the background `/user` merge, off the lock.
        self.spawn_user_info_enrichment(auth.clone());

        echo_agent_telemetry::unified_log::info(
            "auth: devbox recovery succeeded",
            None,
            Some(serde_json::json!({
                "user_id": auth.user_id,
                "has_refresh_token": auth.refresh_token.is_some(),
                "expires_at": auth.expires_at.map(|e| e.to_rfc3339()),
            })),
        );

        Ok(auth)
    }

    /// Return the current valid token string, or an error.
    pub(crate) async fn get_valid_token(self: &Arc<Self>) -> Result<String, AuthError> {
        self.auth().await.map(|a| a.key)
    }

    // ── Refresh chain (single mutation point) ─────────────────────────

    /// The only mutation point: persists on success, records the verdict on failure.
    /// `_lock` type-enforces that the persisting `update()` runs under the file lock.
    async fn apply_refresh_outcome(
        self: &Arc<Self>,
        outcome: RefreshOutcome,
        reason: RefreshReason,
        attempted_key: Option<String>,
        _lock: &AuthFileLock,
    ) -> Result<EchoAgentAuth, AuthError> {
        let pre_key_suffix = attempted_key.as_deref().map(bearer_suffix);
        match outcome {
            RefreshOutcome::Success(new_auth) => match self.update(*new_auth).await {
                Ok(auth) => {
                    let new_suffix = bearer_suffix(&auth.key);
                    echo_agent_telemetry::unified_log::info(
                        "auth.refresh.success",
                        None,
                        Some(serde_json::json!({
                            "expires_at": auth.expires_at.map(|e| e.to_rfc3339()),
                            "old_key_prefix": pre_key_suffix,
                            "new_key_prefix": new_suffix,
                            "key_changed": pre_key_suffix != Some(new_suffix),
                        })),
                    );
                    tracing::info!(expires_at = ?auth.expires_at, "auth.refresh.success");
                    self.refresh_notify.notify_waiters();
                    Ok(auth)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "auth: failed to persist refreshed token");
                    echo_agent_telemetry::unified_log::warn(
                        "auth.refresh.persist_failed",
                        None,
                        Some(serde_json::json!({ "error": format!("{e}") })),
                    );
                    Err(AuthError::transient_source(e))
                }
            },
            RefreshOutcome::PermanentFailure {
                error,
                tried_key,
                tried_refresh_token,
            } => {
                tracing::warn!(reason = ?error.reason, "auth.refresh.permanent_failure");
                echo_agent_telemetry::unified_log::warn(
                    "auth.refresh.permanent_failure",
                    None,
                    Some(serde_json::json!({
                        "reason": format!("{:?}", error.reason),
                    })),
                );
                // A sibling may have successfully refreshed while we got a 401.
                // If disk has a valid token, adopt it instead.
                if let Some(refreshed) = self.try_adopt_disk_token(
                    reason,
                    "auth: adopted sibling token after PermanentFailure",
                ) {
                    return Ok(refreshed);
                }
                // Client contract: only genuine IdP RT rejection discards.
                // Escalated `Other` / `ClientRejected` retain credentials.
                // When mem and disk RTs diverge, clear only the side that was
                // actually tried so an untried successor RT (e.g. mem after a
                // disk-persist failure) is not wiped by a disk-first invalid_grant.
                let failed_reason = error.reason;
                let is_rtr = failed_reason
                    == crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected;
                if is_rtr {
                    let mem = self.current_or_expired();
                    let disk = self.read_disk_auth();
                    // Diverging RTs mean a sibling rotated while we were in
                    // flight: our RT was superseded, not revoked. Demote
                    // without recording so the next attempt picks up the
                    // sibling's token (no sticky lockout, no credential loss).
                    //
                    // When the refresher told us which RT it spent (every
                    // in-tree OIDC path), compare disk against *that*. The
                    // legacy disk-vs-memory heuristic is only a fallback for
                    // unattributed authorities: it asks the wrong question,
                    // because `resolve_refresh_credential` is disk-first, so
                    // the RT actually spent is usually the disk one and the
                    // comparison collapses to "false" exactly when it matters.
                    //
                    // Both arms and the log below read one `disk` observation.
                    // Re-reading per use lets the decision and the line that
                    // explains it disagree about what disk held.
                    let disk_rt = disk.as_ref().and_then(|d| d.refresh_token.as_deref());
                    let sibling_rotated = match tried_refresh_token.as_deref() {
                        Some(tried_rt) => Self::refresh_token_superseded(disk_rt, tried_rt),
                        None => {
                            tried_key.is_none() && self.sibling_has_different_refresh_token(disk_rt)
                        }
                    };
                    if sibling_rotated {
                        tracing::info!("auth: sibling-rotation detected; demoting to transient");
                        echo_agent_telemetry::unified_log::info(
                            "auth.refresh.sibling_rotation_demoted",
                            None,
                            Some(serde_json::json!({
                                "reason": format!("{failed_reason:?}"),
                                "tried_rt_prefix": tried_refresh_token
                                    .as_deref()
                                    .map(bearer_suffix),
                                "disk_rt_prefix": disk_rt.map(bearer_suffix),
                            })),
                        );
                        return Err(AuthError::transient(format!(
                            "sibling-rotation: {failed_reason:?}"
                        )));
                    }
                    let (clear_mem, clear_disk) = match (tried_key.as_ref(), &mem, &disk) {
                        (Some(tk), m, d) => {
                            let mem_match = m.as_ref().is_some_and(|a| a.key == *tk);
                            let disk_match = d.as_ref().is_some_and(|a| a.key == *tk);
                            if mem_match || disk_match {
                                (mem_match, disk_match)
                            } else {
                                (true, true)
                            }
                        }
                        (None, _, _) => (true, true),
                    };
                    if let Some(key) = tried_key.or(attempted_key) {
                        self.record_permanent_failure(key, error);
                    }
                    let mut disk_mutation = "unchanged";
                    if clear_disk {
                        disk_mutation = match self.write_scope_removal(&self.scope) {
                            Ok(m) => m.label(),
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "auth: failed to clear credentials after permanent refresh failure"
                                );
                                "write_failed"
                            }
                        };
                    }
                    if clear_mem {
                        self.clear_inner();
                    }
                    echo_agent_telemetry::unified_log::warn(
                        "auth: cleared credentials after permanent refresh failure",
                        None,
                        Some(serde_json::json!({
                            "reason": format!("{failed_reason:?}"),
                            "disk_mutation": disk_mutation,
                            "cleared_mem": clear_mem,
                            "cleared_disk": clear_disk,
                        })),
                    );
                } else if let Some(key) = tried_key.or(attempted_key) {
                    self.record_permanent_failure(key, error);
                }
                Err(AuthError::permanent(failed_reason))
            }
            RefreshOutcome::TransientFailure { message } => {
                tracing::warn!(%message, "auth.refresh.transient_failure");
                echo_agent_telemetry::unified_log::warn(
                    "auth.refresh.transient_failure",
                    None,
                    Some(serde_json::json!({ "message": &message })),
                );
                Err(AuthError::transient(message))
            }
        }
    }

    /// Re-read auth.json from disk and update the in-memory cache (used by the
    /// refresh chains). Non-destructive: only updates in-memory if disk has a
    /// different valid token that passes the shared adoption guards in
    /// [`Self::try_use_disk_token`] (a sibling process wrote a fresher one).
    ///
    /// Returns `true` only when in-memory state was actually replaced, so
    /// callers can log adoption truthfully instead of inferring it from
    /// "we have a token now" — which is also true when our own token was fine
    /// all along, and made the proactive-refresh log actively misleading when
    /// reconstructing a rotation chain after an incident.
    pub(crate) fn pick_up_sibling_token(&self) -> bool {
        let auth = match read_auth_json(&self.path) {
            Ok(map) => lookup_auth(&map, &self.scope),
            _ => None,
        };
        // Same-key disk state is "nothing to adopt" under this caller's
        // contract (return `true` only on an actual replacement), not a
        // decline — filtered before the shared path so it never logs as one.
        let Some(auth) = auth.filter(|a| self.is_different_token(a)) else {
            return false;
        };
        // Shared enforcement point: expiry, the lagging-mint guard, and the
        // `hot_swap` all live in `try_use_disk_token`, so this path can no
        // longer replace a newer in-memory mint with an older disk token or
        // bypass the sticky-verdict handling in the shared swap.
        match self.try_use_disk_token(Some(&auth), RefreshReason::PreRequest) {
            Ok(adopted) => {
                echo_agent_telemetry::unified_log::info(
                    "auth: pick_up_sibling_token adopted",
                    None,
                    Some(serde_json::json!({
                        "adopted_key_prefix": bearer_suffix(&adopted.key),
                        "expires_at": adopted.expires_at.map(|e| e.to_rfc3339()),
                        "rt_prefix": adopted.refresh_token.as_deref().map(bearer_suffix),
                    })),
                );
                true
            }
            Err(decline) => {
                // `auth()` calls this per-request: a persistently lagging or
                // expired disk token would spam unified_log from here, so
                // declines carry their name at trace level only.
                tracing::debug!(
                    decline = decline.as_str(),
                    "auth: sibling disk token declined"
                );
                false
            }
        }
    }

    /// Check if a candidate auth has a different token than what's in memory.
    pub(crate) fn is_different_token(&self, candidate: &EchoAgentAuth) -> bool {
        let current_key = self.inner.read().as_ref().map(|a| a.key.clone());
        current_key.as_deref() != Some(&candidate.key)
    }

    /// Record a permanent-failure verdict scoped to `token_key` (the rejected
    /// credential).
    pub(crate) fn record_permanent_failure(
        &self,
        token_key: String,
        error: crate::auth::error::RefreshTokenFailedError,
    ) {
        // Don't advertise a TTL for a sticky (never-expiring) verdict.
        let ttl_seconds = (!error.reason.is_sticky()).then(|| PERMANENT_FAILURE_TTL.as_secs());
        echo_agent_telemetry::unified_log::warn(
            "auth.permanent_failure.set",
            None,
            Some(serde_json::json!({
                "reason": format!("{:?}", error.reason),
                "message": error.reason.user_message(),
                "ttl_seconds": ttl_seconds,
            })),
        );
        *self.permanent_failure.write() = Some(ScopedRefreshFailure {
            token_key,
            error,
            recorded_at: DualClock::now(),
        });
    }

    /// Key the sticky verdict is scoped to: the credential a refresh for
    /// `reason` would send, via the shared [`resolve_refresh_credential`] (so
    /// record and check can't drift). Does a synchronous `auth.json` read; that
    /// read is load-bearing (it detects a sibling's freshly rotated token, so an
    /// in-memory-only check could leave a stale verdict on a now-valid
    /// credential). Called from [`Self::permanent_failure`] (only when a verdict
    /// is stored) and once per active `refresh_chain` as the fallback verdict
    /// key; both are pre-IdP paths where the read cost is bounded.
    fn attempted_verdict_key(&self, reason: RefreshReason) -> Option<String> {
        resolve_refresh_credential(self, self.read_disk_auth_silent(), reason).map(|a| a.key)
    }

    /// Sticky verdict for the *attempted* credential, or `None` once it changes
    /// (key mismatch) or, for the recoverable reasons, ages out past
    /// [`PERMANENT_FAILURE_TTL`]. Reads the stored verdict first (cheap lock):
    /// the common no-verdict case returns before any disk I/O; only a stored
    /// verdict triggers [`Self::attempted_verdict_key`]'s disk read.
    ///
    /// After a permanent failure **discards** credentials, sticky reasons
    /// (`RefreshTokenRejected`) still short-circuit with no live credential so
    /// concurrent callers cannot re-hit the IdP with a dead RT. Login
    /// (`hot_swap` / `update`) and logout clear the verdict.
    ///
    /// Sticky applies only to the **same** rejected key or to **no** live
    /// credential (post-discard). A different attempted key (sibling RT/AT
    /// on disk) must be allowed to refresh — otherwise a hard-expired sibling
    /// AT strands a process that could still refresh a live RT.
    ///
    /// TTL expiry is judged on *both* clocks (see [`DualClock`]): the monotonic
    /// clock pauses during a system suspend, so a wall-clock arm is required
    /// for the TTL to elapse across sleep. Without it, a recoverable failure
    /// cached just before the lid closes (e.g. a transient escalation while
    /// the network was already down) would keep short-circuiting `auth()` —
    /// surfacing "run /login" — for up to 5 *awake* minutes after wake, even
    /// though the blip is long over. A genuine revocation simply re-caches on
    /// the next refresh attempt, so expiring "early" costs one IdP roundtrip.
    pub(crate) fn permanent_failure(&self) -> Option<AuthError> {
        let (token_key, reason) = {
            let guard = self.permanent_failure.read();
            let pf = guard.as_ref()?;
            if !pf.error.reason.is_sticky() {
                let (mono, wall) = pf.recorded_at.elapsed();
                if mono >= PERMANENT_FAILURE_TTL || wall >= PERMANENT_FAILURE_TTL {
                    return None;
                }
            }
            (pf.token_key.clone(), pf.error.reason)
        };
        // Verdict exists: confirm it still scopes to the credential a refresh
        // would attempt. Guard dropped above so `inner` isn't co-held.
        // Deliberately `ServerRejected` (the widest resolution) regardless of
        // the caller's reason, so the read never misses a stored verdict.
        match self.attempted_verdict_key(RefreshReason::ServerRejected) {
            Some(k) if k == token_key => Some(AuthError::permanent(reason)),
            // Different credential key: never sticky-block a sibling RT.
            Some(_) => None,
            // No live credential after discard: sticky short-circuit until login.
            None if reason.is_sticky() => Some(AuthError::permanent(reason)),
            None => None,
        }
    }

    /// `true` iff [`Self::permanent_failure`] has a non-expired entry. Lets
    /// callers peek the IdP verdict without touching its `message` payload.
    pub(crate) fn has_permanent_failure(&self) -> bool {
        self.permanent_failure().is_some()
    }

    /// Whether the only way back is a manual `/login`: a sticky IdP
    /// rejection of the refresh token, or no refresh authority/refreshable
    /// credential at all. `false` for anything that self-heals (transient
    /// failures, recoverable verdicts). A *live state* query ("can a future
    /// refresh succeed?"), deliberately separate from
    /// `recovery::manual_auth_reason` (which buckets a terminal error
    /// *value* for the KPI). Drives the "`/login` banner vs self-healing"
    /// decision.
    pub(crate) fn requires_manual_reauth(&self) -> bool {
        use crate::auth::error::RefreshTokenError;
        if let Some(AuthError::Refresh(RefreshTokenError::Permanent(e))) = self.permanent_failure()
            && e.reason.blocks_unattended_retry()
        {
            return true;
        }
        // No refresh authority wired (static-key manager) → nothing can heal
        // an expired credential silently.
        if !self.has_refresher_attached() {
            return true;
        }
        // A refreshable in-memory credential (OIDC RT / external binary) or a
        // sibling's RT on disk lets a later refresh succeed without the user.
        let mem_refreshable = self.token_type().is_refreshable();
        let disk_refreshable = self
            .read_disk_auth_silent()
            .is_some_and(|a| a.refresh_token.is_some());
        !(mem_refreshable || disk_refreshable)
    }

    fn is_external_provider_refresh_authority(&self) -> bool {
        self.echo_agent_com_config.auth_provider_command.is_some()
            && self.token_type() == TokenType::ExternalBinary
    }

    /// `true` iff a [`TokenRefresher`] is wired in. `false` for static-key
    /// or pre-`configure_refresher` managers.
    pub(crate) fn has_refresher_attached(&self) -> bool {
        self.refresher.read().is_some()
    }

    /// Test-only: age the cached `permanent_failure` past its TTL so
    /// the `permanent_failure()` getter treats it as expired.
    #[cfg(test)]
    pub(crate) fn force_permanent_failure_aged_out(&self) {
        if let Some(pf) = self.permanent_failure.write().as_mut() {
            let past_ttl = PERMANENT_FAILURE_TTL + StdDuration::from_secs(1);
            // `checked_sub`: a bare `Instant - Duration` panics on a machine
            // whose monotonic clock hasn't been up for `past_ttl` yet (fresh
            // boot / fresh VM). Falling back to `now` leaves the verdict live,
            // which the asserting test will surface loudly.
            let now_mono = std::time::Instant::now();
            let now_wall = std::time::SystemTime::now();
            pf.recorded_at = DualClock {
                mono: now_mono.checked_sub(past_ttl).unwrap_or(now_mono),
                wall: now_wall.checked_sub(past_ttl).unwrap_or(now_wall),
            };
        }
    }

    /// Test-only: simulate a system suspend between recording and reading the
    /// cached `permanent_failure` — the monotonic clock stays fresh while the
    /// wall clock is rewound past the TTL (a suspend pauses the monotonic
    /// clock, so on wake `mono` reads short while `wall` reads long).
    #[cfg(test)]
    pub(crate) fn force_permanent_failure_wall_aged_out(&self) {
        if let Some(pf) = self.permanent_failure.write().as_mut() {
            let now = std::time::SystemTime::now();
            pf.recorded_at.wall = now
                .checked_sub(PERMANENT_FAILURE_TTL + StdDuration::from_secs(1))
                .unwrap_or(now);
        }
    }

    // ── 401 recovery entry point ──────────────────────────────────────

    /// 401 recovery state machine driven by the `rejected` credential. For
    /// one-shot recovery off the live bearer, use `try_recover_unauthorized()`.
    pub(crate) fn unauthorized_recovery(
        self: &Arc<Self>,
        rejected: Option<EchoAgentAuth>,
        source: crate::auth::recovery::RecoverySource,
    ) -> crate::auth::recovery::UnauthorizedRecovery {
        crate::auth::recovery::UnauthorizedRecovery::new(self.clone(), rejected, source)
    }

    /// 401 recovery off the live bearer. Snapshots the rejected credential once
    /// for KPI attribution. On **transient** refresh failure (network, 5xx,
    /// sleep/dark-wake defer, lock timeout) retries with backoff before giving
    /// up. Permanent failures and NotLoggedIn stop immediately.
    ///
    /// After a successful recovery the **caller** retries the original request
    /// (turn-level may resubmit more than once; API resubmit is separate from
    /// refresh retries).
    pub(crate) async fn try_recover_unauthorized(
        self: &Arc<Self>,
        source: crate::auth::recovery::RecoverySource,
    ) -> bool {
        /// Bounded refresh attempts for non-permanent failures. Kept strictly
        /// below OidcRefresher's consecutive-transient escalation threshold so
        /// one 401 recovery cannot alone escalate a network blip to permanent
        /// `Other`.
        const MAX_TRANSIENT_ATTEMPTS: u32 = 2;

        let cached = self.with_inner_read(|inner| inner.cloned());
        let mut delay = StdDuration::from_millis(500);
        for attempt in 0..MAX_TRANSIENT_ATTEMPTS {
            match self
                .unauthorized_recovery(cached.clone(), source)
                .next()
                .await
            {
                Ok(_) => return true,
                Err(e) if e.is_transient() && attempt + 1 < MAX_TRANSIENT_ATTEMPTS => {
                    echo_agent_telemetry::unified_log::warn(
                        "auth recovery: transient failure, retrying",
                        None,
                        Some(serde_json::json!({
                            "attempt": attempt + 1,
                            "max_attempts": MAX_TRANSIENT_ATTEMPTS,
                            "delay_ms": delay.as_millis() as u64,
                            "error": format!("{e}"),
                        })),
                    );
                    tokio::time::sleep(delay).await;
                    delay = (delay.saturating_mul(2)).min(StdDuration::from_secs(4));
                }
                Err(_) => return false,
            }
        }
        false
    }

    pub(crate) fn record_manual_auth(
        &self,
        snapshot: &crate::auth::recovery::RejectedAuth,
        err: &AuthError,
        trigger: ManualAuthSurface,
    ) {
        self.manual_auth.record(snapshot, err, trigger);
    }

    #[cfg(test)]
    pub(crate) fn manual_auth_last_token(&self) -> Option<String> {
        self.manual_auth.last_token_for_test()
    }

    #[cfg(test)]
    pub(crate) fn manual_auth_last_emit(&self) -> Option<echo_agent_telemetry::events::ManualAuth> {
        self.manual_auth.last_emit_for_test()
    }

    // ── Proactive refresh ─────────────────────────────────────────────

    /// Spawn a background task that proactively refreshes the token
    /// ahead of expiry. Cancelled via `cancel`.
    ///
    /// Idempotent: a second call on the same `Arc` is a no-op (debug
    /// log + return). Sleep duration and back-off conditions are
    /// computed by [`compute_proactive_sleep`]; see its body for the
    /// six non-busy-loop guards (permanent_failure, non-refreshable
    /// type, no refresher, sleep-gated, dark wake with a wire-valid
    /// token, no expires_at).
    pub(crate) fn start_proactive_refresh(self: &Arc<Self>, cancel: CancellationToken) {
        use std::sync::atomic::Ordering;
        // AcqRel/Acquire publishes the spawned task's captured Arc to
        // any thread that observes the bool as `true`; SeqCst would also
        // be correct, just slower.
        if self
            .proactive_started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            tracing::debug!("auth: start_proactive_refresh already running on this Arc, ignoring");
            return;
        }
        #[cfg(test)]
        self.proactive_starts.fetch_add(1, Ordering::SeqCst);
        let this = self.clone();
        tokio::spawn(async move {
            // Consecutive failed `auth()` attempts in THIS loop, driving
            // [`proactive_failure_backoff`]: `compute_proactive_sleep` returns
            // 0 for any past-refresh-point token regardless of how the last
            // attempt failed, so without this a hard-expired token during an
            // outage spins with zero delay. Reset on success or adoption.
            let mut consecutive_failures: u32 = 0;
            loop {
                let sleep_dur = compute_proactive_sleep(&this)
                    .max(proactive_failure_backoff(consecutive_failures));

                tokio::select! {
                    _ = cancel.cancelled() => {
                        tracing::debug!("auth: proactive refresh task cancelled");
                        return;
                    }
                    _ = tokio::time::sleep(sleep_dur) => {}
                    // OS wake: re-evaluate immediately (see `wake_notify`).
                    // The failure ladder resets too — a wake is a changed
                    // world that deserves the fast schedule, not a backoff
                    // cap accumulated across overnight dark-wake misses.
                    _ = this.wake_notify.notified() => {
                        consecutive_failures = 0;
                        tracing::debug!("auth: proactive refresh re-armed by OS wake");
                    }
                }

                #[cfg(test)]
                this.proactive_iter_count.fetch_add(1, Ordering::SeqCst);

                // Re-check the back-off preconditions after the sleep so
                // a concurrent `update()` / `hot_swap()` /
                // `configure_refresher()` is observed before we attempt
                // `auth()`.
                if this.permanent_failure().is_some() {
                    // Try disk adoption — a sibling may have refreshed.
                    if let Some(_refreshed) = this.try_adopt_disk_token(
                        RefreshReason::PreRequest,
                        "auth: proactive refresh adopted sibling token during PermanentFailure",
                    ) {
                        // Fall through to the normal proactive-refresh sleep
                        // calculation which will schedule the next refresh
                        // based on the adopted token's expiry.
                        consecutive_failures = 0;
                        continue;
                    }
                    tracing::debug!(
                        "auth: skipping proactive refresh, permanent failure still set"
                    );
                    continue;
                }
                if !this.token_type().is_refreshable() {
                    tracing::debug!(
                        "auth: skipping proactive refresh, token type is not refreshable"
                    );
                    continue;
                }
                if this.refresher.read().is_none() {
                    tracing::debug!("auth: skipping proactive refresh, no refresher configured");
                    continue;
                }

                // Before calling the IdP, check if a sibling process
                // already refreshed and wrote a valid token to disk.
                // Combined with jitter, the first process to wake
                // refreshes; later processes adopt the result here.
                let adopted_from_sibling = this.pick_up_sibling_token();
                if this.current().is_some() {
                    let adopted = this.current().map(|a| bearer_suffix(&a.key).to_owned());
                    let expires_at = this
                        .inner
                        .read()
                        .as_ref()
                        .and_then(|a| a.expires_at.map(|e| e.to_rfc3339()));
                    // Distinguish "a sibling's token replaced ours" from "our
                    // own token is still valid". Both skip the refresh, but
                    // conflating them makes the log actively misleading when
                    // reconstructing a rotation chain after an incident.
                    if adopted_from_sibling {
                        tracing::info!(
                            "auth: proactive refresh skipped, adopted sibling token from disk"
                        );
                    } else {
                        tracing::info!(
                            "auth: proactive refresh skipped, in-memory token still valid"
                        );
                    }
                    echo_agent_telemetry::unified_log::info(
                        "auth: proactive refresh skipped",
                        None,
                        Some(serde_json::json!({
                            "adopted_from_sibling": adopted_from_sibling,
                            "key_prefix": adopted,
                            "expires_at": expires_at,
                        })),
                    );
                    consecutive_failures = 0;
                    continue;
                }

                tracing::info!("auth: proactive refresh starting");
                match this.auth().await {
                    Ok(auth) => {
                        consecutive_failures = 0;
                        tracing::info!("auth: proactive refresh succeeded");
                        echo_agent_telemetry::unified_log::info(
                            "auth: proactive refresh completed",
                            None,
                            Some(serde_json::json!({
                                "result": "success",
                                "key_prefix": bearer_suffix(&auth.key),
                                "expires_at": auth.expires_at.map(|e| e.to_rfc3339()),
                            })),
                        );
                    }
                    Err(e) => {
                        consecutive_failures = consecutive_failures.saturating_add(1);
                        tracing::warn!(error = %e, "auth: proactive refresh failed");
                        echo_agent_telemetry::unified_log::warn(
                            "auth: proactive refresh completed",
                            None,
                            Some(serde_json::json!({
                                "result": "failed",
                                "consecutive_failures": consecutive_failures,
                                "backoff_ms": proactive_failure_backoff(consecutive_failures)
                                    .as_millis() as u64,
                                "error": format!("{e}"),
                            })),
                        );
                    }
                }
            }
        });
    }
}

/// Backoff after `n` consecutive failed proactive refresh attempts:
/// 0 → none, then 5 s · 2^(n−1) capped at [`BACKOFF_INTERVAL`], plus 0–3 s
/// jitter to de-stagger siblings that failed in lockstep. Sized so the OIDC
/// transient-escalation threshold cannot be reached inside a typical
/// post-wake network-recovery window.
pub(crate) fn proactive_failure_backoff(consecutive_failures: u32) -> StdDuration {
    if consecutive_failures == 0 {
        return StdDuration::ZERO;
    }
    let exp = consecutive_failures.saturating_sub(1).min(6); // 5s..320s pre-cap
    let base = StdDuration::from_secs(5)
        .saturating_mul(1u32 << exp)
        .min(BACKOFF_INTERVAL);
    base + StdDuration::from_millis(rand::random_range(0..3000))
}

/// Floor for the proactive loop's per-iteration sleep. Past the refresh
/// point the schedule returns "now", and the adopt/skip `continue` paths
/// re-roll the jitter each pass — a raw zero sleep spins that into
/// thousands of 1–2 ms iterations inside the 0–60 s jitter window. One
/// second bounds the spin without meaningfully delaying a due refresh
/// (the schedule runs off a 5-minute buffer).
pub(crate) const PROACTIVE_MIN_SLEEP: StdDuration = StdDuration::from_secs(1);

/// Compute the sleep duration for the next iteration of the proactive
/// refresh loop. Pulled out of `start_proactive_refresh` so the gate
/// chain is testable in isolation and the spawned async block stays small.
pub(crate) fn compute_proactive_sleep(this: &AuthManager) -> StdDuration {
    if this.permanent_failure().is_some() {
        // A previous refresh failed permanently and the verdict is cached.
        // `auth()` short-circuits every iteration -- back off until the cancel
        // token fires or a fresh `update()` / `hot_swap()` clears it. Jitter so
        // a synchronized fleet event (mass client rotation) doesn't re-hit the
        // IdP in lockstep once the recoverable verdicts age out.
        return BACKOFF_INTERVAL
            + StdDuration::from_secs(rand::random_range(0..JITTER_RANGE_SECS) as u64);
    }
    if !this.token_type().is_refreshable() {
        // `ApiKey`, `LegacySession`, and `None` cannot be refreshed
        // silently. Without this gate, an expired token of these types
        // produces `sleep_dur=0` -> `auth()` -> `TokenExpiredNoRefresh`
        // -> repeat at 100% CPU and log spam (same shape as the pre-fix
        // permanent-failure busy-loop).
        return BACKOFF_INTERVAL;
    }
    if this.refresher.read().is_none() {
        // Defensive: if `start_proactive_refresh` outraced
        // `configure_refresher` at startup, `auth()` would emit a transient
        // "no refresher configured" error every iteration (and busy-loop for
        // a past-expiry token). Hold off here until the refresher is installed.
        return BACKOFF_INTERVAL;
    }
    if this.is_sleep_gated() {
        // System sleep is imminent: `refresh_chain` defers every attempt, so
        // an expired token would otherwise busy-loop here (`sleep_dur=0` ->
        // `auth()` -> transient defer -> repeat). Back off until the gate
        // clears on wake or auto-expires (`SLEEP_GATE_MAX`).
        return BACKOFF_INTERVAL;
    }
    // Dark wake with a wire-valid token: `refresh_chain` would defer, so
    // back off instead of busy-looping against the deferral. With nothing
    // usable to serve it no longer defers — fall through to the expiry
    // schedule rather than strand the session for a full interval.
    if this.current_wire_valid().is_some() && this.is_dark_wake() {
        return BACKOFF_INTERVAL;
    }
    match this.inner.read().as_ref().and_then(|a| a.expires_at) {
        Some(expires_at) => {
            let buffer = early_invalidation();
            // Add random jitter (0–60 s) so sibling processes don't all
            // wake at the same instant and thundering-herd the IdP. The
            // first process to wake refreshes and writes to disk; later
            // processes pick up the sibling token via
            // `pick_up_sibling_token` at the top of the loop.
            let jitter = Duration::seconds(rand::random_range(0..JITTER_RANGE_SECS));
            let target = expires_at - buffer - jitter;
            let delta = target.signed_duration_since(Utc::now());
            if delta <= Duration::zero() {
                // Already past the early-invalidation boundary: `auth()`
                // will enter `refresh_chain` on the next pass. Floored
                // (never a raw zero) so the adopt/skip `continue` paths
                // cannot spin -- see [`PROACTIVE_MIN_SLEEP`].
                PROACTIVE_MIN_SLEEP
            } else {
                // The earlier `delta <= 0` branch already handled the
                // negative case, so `to_std` cannot fail here. expect
                // surfaces a clear panic message if a future change
                // breaks the invariant.
                delta
                    .to_std()
                    .expect("delta > 0 above; chrono::Duration -> std::Duration must succeed")
                    .max(PROACTIVE_MIN_SLEEP)
            }
        }
        // No expires_at (typical for external binaries): poll every
        // BACKOFF_INTERVAL. Operators wanting tighter feedback set
        // `[echo_agent_com] auth_token_ttl` to drive a real schedule.
        None => BACKOFF_INTERVAL,
    }
}

/// Tools + pager voice bearer. Static: env → process model key → disk.
/// Kill-switch / `preferred_method = oidc` block static keys.
pub(crate) struct SharedAuthKeyProvider(pub Arc<AuthManager>);

impl echo_agent_tools::types::ApiKeyProvider for SharedAuthKeyProvider {
    fn current_api_key(&self) -> Option<String> {
        if prefers_static_api_key(&self.0) {
            return resolve_static_api_key(&self.0);
        }
        // Wire-valid session > static key > expired session (last resort).
        // Hard expiry, not the refresh buffer: sync cannot refresh, so a
        // buffered-but-valid token must still beat static.
        self.0
            .current_wire_valid()
            .map(|a| a.key)
            .or_else(|| resolve_static_api_key(&self.0))
            .or_else(|| self.0.current_or_expired().map(|a| a.key))
    }

    fn current_api_key_async(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send + '_>> {
        let am = self.0.clone();
        Box::pin(async move {
            if prefers_static_api_key(&am) {
                return resolve_static_api_key(&am);
            }
            am.get_valid_token()
                .await
                .ok()
                .or_else(|| resolve_static_api_key(&am))
        })
    }
}

fn prefers_static_api_key(am: &AuthManager) -> bool {
    matches!(
        am.echo_agent_com_config.preferred_method,
        Some(super::config::PreferredAuthMethod::ApiKey)
    )
}

/// Env → process model key → disk. Off under kill-switch / oidc pin.
fn resolve_static_api_key(am: &AuthManager) -> Option<String> {
    if am.echo_agent_com_config.api_key_auth_disabled() {
        return None;
    }
    if matches!(
        am.echo_agent_com_config.preferred_method,
        Some(super::config::PreferredAuthMethod::Oidc)
    ) {
        return None;
    }
    non_empty_key(crate::agent::auth_method::read_echo_agent_api_key_env().ok())
        .or_else(|| non_empty_key(am.process_static_api_key.read().clone()))
        .or_else(|| am.cached_disk_api_key())
}

fn api_key_from_auth_file(path: &Path) -> Option<String> {
    let map = read_auth_json(path).ok()?;
    non_empty_key(map.get(super::model::API_KEY_SCOPE).map(|a| a.key.clone()))
}

/// Memo for [`AuthManager::cached_disk_api_key`]. `stamp == None` = file absent.
struct StaticKeyCacheEntry {
    stamp: Option<AuthFileStamp>,
    key: Option<String>,
}

/// (inode, mtime, len). `write_auth_json`'s temp+rename allocates a new inode
/// per rewrite, so even a same-length same-mtime rewrite misses the memo.
/// Windows has no stable inode (0 there); its fine mtimes suffice.
type AuthFileStamp = (u64, Option<std::time::SystemTime>, u64);

fn auth_file_stamp(path: &Path) -> Option<AuthFileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(&meta);
    #[cfg(not(unix))]
    let ino = 0;
    Some((ino, meta.modified().ok(), meta.len()))
}

impl AuthManager {
    /// `echoagent::api_key` from this manager's auth file, memoized on
    /// [`AuthFileStamp`]: bearer resolution runs per tool call, so this
    /// costs a `stat` instead of a read+parse on the hot path.
    fn cached_disk_api_key(&self) -> Option<String> {
        let stamp = auth_file_stamp(&self.path);
        let mut cache = self.static_key_cache.lock();
        match cache.as_ref() {
            Some(entry) if entry.stamp == stamp => entry.key.clone(),
            _ => {
                let key = stamp
                    .is_some()
                    .then(|| api_key_from_auth_file(&self.path))
                    .flatten();
                *cache = Some(StaticKeyCacheEntry {
                    stamp,
                    key: key.clone(),
                });
                key
            }
        }
    }

    /// Set the process model key (empty clears). Not for session tokens.
    pub(crate) fn set_process_static_api_key(&self, key: Option<String>) {
        let key = key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty());
        *self.process_static_api_key.write() = key;
    }

    /// Static/BYOK key for export paths (e.g. desktop `getBearerToken`). Never a
    /// session JWT; respects kill-switch and preferred-method pin.
    pub(crate) fn static_api_key_for_export(&self) -> Option<String> {
        resolve_static_api_key(self)
    }
}

fn non_empty_key(key: Option<String>) -> Option<String> {
    key.map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

/// Per-request bearer for out-of-crate consumers (e.g. pager voice).
pub fn shared_api_key_provider(
    auth_manager: Arc<AuthManager>,
) -> echo_agent_tools::types::SharedApiKeyProvider {
    Arc::new(SharedAuthKeyProvider(auth_manager))
}

/// Compile-time check that `AuthManager` is `Send + Sync` (so the
/// proactive refresh task and arbitrary `Arc<AuthManager>` consumers can
/// safely cross a multi-threaded executor / thread boundary). A future
/// refactor that adds a `!Send` field would otherwise fail to compile in
/// `tokio::spawn(... this.clone() ...)` with a confusing trait-bound
/// error far from the offending field.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AuthManager>();
};

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
