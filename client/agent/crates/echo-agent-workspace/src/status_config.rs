//! Runtime-tunable timing/threshold config for the workspace tool server.
//!
//! All values are read once at startup from `ECHO_AGENT_WORKSPACE_*` environment
//! variables via [`StatusConfig::from_env`]. Unset or unparseable variables
//! fall back to the documented defaults (with a `warn!` on parse failure), so
//! construction never fails.

use std::str::FromStr;
use std::time::Duration;

// ── Default timing/threshold values ──────────────────────────────────────
// Single source of truth for the `StatusConfig::default()` values and the
// documented fallbacks for each `ECHO_AGENT_WORKSPACE_*` env var.

/// Default interval between status/heartbeat emissions.
const DEFAULT_HEARTBEAT_SECS: u64 = 30;
/// Default transport keepalive interval. Should exceed `heartbeat`.
const DEFAULT_KEEPALIVE_SECS: u64 = 60;
/// Default WebSocket keepalive ping cadence for the server SDK connection.
const DEFAULT_WS_PING_SECS: u64 = 30;
/// Default number of consecutive hub reconnect failures before warning.
const DEFAULT_HUB_WARN_THRESHOLD: u32 = 5;
/// Default base delay (ms) for exponential backoff on failed hub sends.
const DEFAULT_HUB_BACKOFF_BASE_MS: u64 = 100;
/// Default idle window (s) after which an inactive session is pruned. Kept
/// aligned with the sandbox service's idle-hibernate grace.
const DEFAULT_SESSION_IDLE_PRUNE_SECS: u64 = 1800;
/// Default max time (s) to wait for in-flight work to drain on shutdown.
const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 30;
/// Default per-call timeout (s) for agent gRPC RPCs.
const DEFAULT_AGENT_RPC_TIMEOUT_SECS: u64 = 30;
/// Default timeout (s) for establishing an agent connection.
const DEFAULT_AGENT_CONNECT_TIMEOUT_SECS: u64 = 5;
/// Default preview-activity withhold window. Sourced from the tracker's
/// `PREVIEW_ACTIVITY_WINDOW_MS` so the two can't drift.
const DEFAULT_PREVIEW_ACTIVITY_WINDOW_MS: u64 = crate::activity::PREVIEW_ACTIVITY_WINDOW_MS;
/// Default client-RPC withhold window; `0` disables the withhold.
const DEFAULT_RPC_ACTIVITY_WINDOW_MS: u64 = crate::activity::RPC_ACTIVITY_WINDOW_MS;
/// Default client-presence withhold window when the keepalive is enabled.
const DEFAULT_PRESENCE_ACTIVITY_WINDOW_MS: u64 = crate::activity::PRESENCE_ACTIVITY_WINDOW_MS;
/// Default preview-activity scrape cadence. Must stay below the withhold window.
const DEFAULT_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS: u64 = 10_000;
/// Smallest window that still leaves room for a strictly-smaller scrape; only a
/// broken config reaches it (the normal window is 60s).
const MIN_PREVIEW_ACTIVITY_WINDOW_MS: u64 = 2;
/// Scrape-interval floor; `0` would busy-loop the scraper.
const MIN_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS: u64 = 1;
/// Ceiling on the client-RPC withhold window, so a seconds-for-ms typo cannot
/// pin a sandbox for a day. `0` (the kill switch) is exempt.
const MAX_RPC_ACTIVITY_WINDOW_MS: u64 = 600_000;
/// Ceiling on the client-presence withhold window; `0` is exempt.
const MAX_PRESENCE_ACTIVITY_WINDOW_MS: u64 = 600_000;
/// Default keep-awake window for scheduled tasks; `0` turns it off.
const DEFAULT_SCHEDULED_TASK_KEEP_AWAKE_MS: u64 =
    crate::activity::SCHEDULED_TASK_KEEP_AWAKE_WINDOW_MS;
/// Ceiling on the keep-awake window; `0` is exempt. Matches the 7-day cap
/// on session TTL overrides.
const MAX_SCHEDULED_TASK_KEEP_AWAKE_MS: u64 = 7 * 24 * 3_600_000; // 7 days
const DEFAULT_PREVIEW_STATE_POLL_INTERVAL_MS: u64 = 5_000;
/// Poll-interval floor; `0` would busy-loop the watcher against loopback.
/// Doubles as the gap floor between consecutive long-poll requests in
/// `crate::preview_state`, so a proxy that ignores `?wait` can't be hot-looped.
pub(crate) const MIN_PREVIEW_STATE_POLL_INTERVAL_MS: u64 = 100;
/// Default preview-state long-poll hold; `0` disables long-polling entirely
/// (the watcher keeps today's fixed-interval cadence).
const DEFAULT_PREVIEW_STATE_WAIT_SECS: u64 = 0;
/// Ceiling on the long-poll hold, mirroring the proxy's own `?wait` clamp
/// (`echo-agent-preview-proxy` clamps held requests to 15s).
const MAX_PREVIEW_STATE_WAIT_SECS: u64 = 15;
/// Default preview-proxy discovery refresh passthrough; `0` means the
/// supervisor omits `--discovery-refresh-ms` and the proxy uses its default.
const DEFAULT_PREVIEW_DISCOVERY_REFRESH_MS: u64 = 0;
/// Discovery-refresh floor, mirroring the proxy's own flag floor; anything
/// lower would rescan `/proc/net/tcp` in a near-busy loop.
const MIN_PREVIEW_DISCOVERY_REFRESH_MS: u64 = 100;
/// Discovery-refresh ceiling: past 10s the preview-state document goes stale
/// enough to defeat the reporter, so a seconds-for-ms typo is repaired.
const MAX_PREVIEW_DISCOVERY_REFRESH_MS: u64 = 10_000;
/// Default fraction of TTL (or remaining lifetime at cold start) at which to
/// refresh. Must stay in (0, 1).
const DEFAULT_OIDC_REFRESH_FRACTION: f64 = 0.6;
/// Default half-width of the jitter window as a fraction of the schedule
/// scale (TTL or remaining). Must stay in [0, 0.5].
const DEFAULT_OIDC_REFRESH_JITTER_FRACTION: f64 = 0.2;
/// Default hard floor before expiry. Must exceed the SDK's 60s reactive
/// margin so a reconnect never has to refresh synchronously.
const DEFAULT_OIDC_SAFETY_MARGIN_SECS: u64 = 120;
/// Default floor between consecutive *successful* refreshes. Must stay below
/// `safety_margin` so a healthy short-TTL token does not hot-loop the IdP.
const DEFAULT_OIDC_MIN_REFRESH_INTERVAL_SECS: u64 = 60;
/// Smallest allowed success-path spacing. Zero would reschedule immediately
/// when TTL ≤ `safety_margin`.
const MIN_OIDC_MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(1);
/// Ceiling on OIDC timing knobs so an env typo cannot overflow date math.
const MAX_OIDC_DURATION: Duration = Duration::from_secs(24 * 3600);

/// Proactive OIDC refresh knobs. `enabled` defaults off.
#[derive(Debug, Clone, PartialEq)]
pub struct ProactiveRefreshConfig {
    /// `ECHO_AGENT_WORKSPACE_OIDC_PROACTIVE_REFRESH_ENABLED`. Default `false`.
    pub enabled: bool,
    /// `ECHO_AGENT_WORKSPACE_OIDC_REFRESH_FRACTION`, open interval `(0, 1)`.
    pub fraction: f64,
    /// `ECHO_AGENT_WORKSPACE_OIDC_REFRESH_JITTER_FRACTION`, closed `[0, 0.5]`.
    pub jitter_fraction: f64,
    /// Hard floor before expiry (`ECHO_AGENT_WORKSPACE_OIDC_REFRESH_SAFETY_MARGIN_SECS`).
    pub safety_margin: Duration,
    /// Floor between consecutive *successful* refreshes
    /// (`ECHO_AGENT_WORKSPACE_OIDC_MIN_REFRESH_INTERVAL_SECS`). Failure retries
    /// are bounded by expiry, not this floor.
    pub min_refresh_interval: Duration,
}

impl Default for ProactiveRefreshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fraction: DEFAULT_OIDC_REFRESH_FRACTION,
            jitter_fraction: DEFAULT_OIDC_REFRESH_JITTER_FRACTION,
            safety_margin: Duration::from_secs(DEFAULT_OIDC_SAFETY_MARGIN_SECS),
            min_refresh_interval: Duration::from_secs(DEFAULT_OIDC_MIN_REFRESH_INTERVAL_SECS),
        }
    }
}

impl ProactiveRefreshConfig {
    /// Populate from `ECHO_AGENT_WORKSPACE_OIDC_*`. Unset or unparseable vars fall
    /// back to the default with a `warn!`. Never fails.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let mut cfg = Self {
            enabled: parse_or(
                "ECHO_AGENT_WORKSPACE_OIDC_PROACTIVE_REFRESH_ENABLED",
                defaults.enabled,
            ),
            fraction: frac_or(
                "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_FRACTION",
                defaults.fraction,
                |v| v > 0.0 && v < 1.0,
            ),
            jitter_fraction: frac_or(
                "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_JITTER_FRACTION",
                defaults.jitter_fraction,
                |v| (0.0..=0.5).contains(&v),
            ),
            safety_margin: secs_or(
                "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_SAFETY_MARGIN_SECS",
                defaults.safety_margin,
            ),
            min_refresh_interval: secs_or(
                "ECHO_AGENT_WORKSPACE_OIDC_MIN_REFRESH_INTERVAL_SECS",
                defaults.min_refresh_interval,
            ),
        };
        cfg.validate();
        cfg
    }

    /// Repair a raw struct literal: finite in-range fractions, nonzero
    /// durations capped at 24h, and `min_refresh_interval < safety_margin`.
    /// The short-TTL success-path floor can still schedule after expiry.
    pub fn validate(&mut self) {
        if !(self.fraction.is_finite() && self.fraction > 0.0 && self.fraction < 1.0) {
            tracing::warn!(
                fraction = self.fraction,
                default = DEFAULT_OIDC_REFRESH_FRACTION,
                "ECHO_AGENT_WORKSPACE OIDC refresh fraction out of range; using default"
            );
            self.fraction = DEFAULT_OIDC_REFRESH_FRACTION;
        }
        if !(self.jitter_fraction.is_finite() && (0.0..=0.5).contains(&self.jitter_fraction)) {
            tracing::warn!(
                jitter_fraction = self.jitter_fraction,
                default = DEFAULT_OIDC_REFRESH_JITTER_FRACTION,
                "ECHO_AGENT_WORKSPACE OIDC jitter fraction out of range; using default"
            );
            self.jitter_fraction = DEFAULT_OIDC_REFRESH_JITTER_FRACTION;
        }
        if self.safety_margin > MAX_OIDC_DURATION {
            tracing::warn!(
                safety_margin = ?self.safety_margin,
                clamped = ?MAX_OIDC_DURATION,
                "ECHO_AGENT_WORKSPACE OIDC safety margin above cap; clamped"
            );
            self.safety_margin = MAX_OIDC_DURATION;
        }
        if self.min_refresh_interval > MAX_OIDC_DURATION {
            tracing::warn!(
                min_refresh_interval = ?self.min_refresh_interval,
                clamped = ?MAX_OIDC_DURATION,
                "ECHO_AGENT_WORKSPACE OIDC min refresh interval above cap; clamped"
            );
            self.min_refresh_interval = MAX_OIDC_DURATION;
        }
        if self.min_refresh_interval < MIN_OIDC_MIN_REFRESH_INTERVAL {
            tracing::warn!(
                min_refresh_interval = ?self.min_refresh_interval,
                floored_to = ?MIN_OIDC_MIN_REFRESH_INTERVAL,
                "ECHO_AGENT_WORKSPACE OIDC min refresh interval below floor; floored"
            );
            self.min_refresh_interval = MIN_OIDC_MIN_REFRESH_INTERVAL;
        }
        if self.min_refresh_interval >= self.safety_margin {
            let shrunk = self.safety_margin.saturating_sub(Duration::from_secs(1));
            if shrunk >= MIN_OIDC_MIN_REFRESH_INTERVAL {
                tracing::warn!(
                    min_refresh_interval = ?self.min_refresh_interval,
                    safety_margin = ?self.safety_margin,
                    repaired_min = ?shrunk,
                    "ECHO_AGENT_WORKSPACE OIDC min refresh interval must be < safety_margin; reduced"
                );
                self.min_refresh_interval = shrunk;
            } else {
                let raised = self.min_refresh_interval + Duration::from_secs(1);
                if raised <= MAX_OIDC_DURATION {
                    tracing::warn!(
                        min_refresh_interval = ?self.min_refresh_interval,
                        safety_margin = ?self.safety_margin,
                        repaired_safety = ?raised,
                        "ECHO_AGENT_WORKSPACE OIDC safety_margin too small to sit above min interval; raised"
                    );
                    self.safety_margin = raised;
                } else {
                    // Raising by 1s would exceed the 24h cap; pin safety at the
                    // cap and drop min so `min < safety` still holds.
                    let repaired_min = MAX_OIDC_DURATION - Duration::from_secs(1);
                    tracing::warn!(
                        min_refresh_interval = ?self.min_refresh_interval,
                        safety_margin = ?self.safety_margin,
                        repaired_min = ?repaired_min,
                        repaired_safety = ?MAX_OIDC_DURATION,
                        "ECHO_AGENT_WORKSPACE OIDC safety_margin cannot rise above the 24h cap; pinned and reduced min"
                    );
                    self.safety_margin = MAX_OIDC_DURATION;
                    self.min_refresh_interval = repaired_min;
                }
            }
        }
    }
}

/// Tunable timing/threshold constants for the workspace tool server.
#[derive(Debug, Clone)]
pub struct StatusConfig {
    /// Interval between status/heartbeat emissions.
    pub heartbeat: Duration,
    /// Transport keepalive interval. Should exceed `heartbeat`.
    pub keepalive: Duration,
    /// WebSocket keepalive ping cadence for the server SDK connection.
    pub ws_ping: Duration,
    /// Reconnect backoff schedule for the server SDK connection. `None` leaves
    /// the SDK's built-in default exponential schedule in place.
    pub ws_reconnect_backoff: Option<Vec<Duration>>,
    /// Optional WebSocket liveness deadline
    /// (`ECHO_AGENT_WORKSPACE_WS_LIVENESS_DEADLINE_SECS`). `None` leaves the SDK's
    /// `min(4× ping, 120s)` default in place.
    pub ws_liveness_deadline: Option<Duration>,
    /// Proactive OIDC refresh policy (`ECHO_AGENT_WORKSPACE_OIDC_*`).
    pub oidc_refresh: ProactiveRefreshConfig,
    /// Number of consecutive server reconnect failures before warning.
    pub hub_warn_threshold: u32,
    /// Base delay for exponential backoff on failed server event-notification sends.
    pub hub_backoff_base: Duration,
    /// Idle duration after which an inactive session is pruned.
    pub session_idle_prune: Duration,
    /// Legacy single-phase drain timeout (`ECHO_AGENT_WORKSPACE_DRAIN_TIMEOUT_SECS`),
    /// retained for compatibility; the SIGTERM and server-evict paths now use the
    /// two-phase drain bounded by `ECHO_AGENT_WORKSPACE_TERMINATION_GRACE_MS`.
    pub drain_timeout: Duration,
    /// Per-call timeout for agent RPCs.
    pub agent_rpc_timeout: Duration,
    /// Timeout for establishing an agent connection.
    pub agent_connect_timeout: Duration,
    /// Opt-in foreground-only idle (`ECHO_AGENT_WORKSPACE_IDLE_IGNORE_BACKGROUND_TASKS`);
    /// requires the literal `"true"` — other spellings fall back to this default.
    pub idle_ignores_background: bool,
    /// Recent preview-proxy traffic withholds idle for this window
    /// (`ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_WINDOW_MS`).
    pub preview_activity_window: Duration,
    /// Cadence at which the preview-activity scraper polls the proxy
    /// (`ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS`); kept strictly
    /// below `preview_activity_window` by [`validate`](Self::validate).
    pub preview_activity_scrape_interval: Duration,
    /// A client mutation RPC withholds idle for this window
    /// (`ECHO_AGENT_WORKSPACE_RPC_ACTIVITY_WINDOW_MS`); zero disables. Clamped to
    /// `MAX_RPC_ACTIVITY_WINDOW_MS` by [`validate`](Self::validate).
    pub rpc_activity_window: Duration,
    /// Presence-keepalive kill-switch
    /// (`ECHO_AGENT_WORKSPACE_PRESENCE_KEEPALIVE_ENABLED`, default OFF). Off ⇒ the
    /// `ClientPresence` tier is wired with a zero window.
    pub presence_keepalive_enabled: bool,
    /// A visible client-presence note withholds idle for this window
    /// (`ECHO_AGENT_WORKSPACE_PRESENCE_ACTIVITY_WINDOW_MS`); zero disables.
    pub presence_activity_window: Duration,
    /// A live scheduled task keeps the sandbox awake while its next run is at most this far away (`ECHO_AGENT_WORKSPACE_SCHEDULED_TASK_KEEP_AWAKE_MS`).
    /// Zero turns it off. Clamped to `MAX_SCHEDULED_TASK_KEEP_AWAKE_MS` by [`validate`](Self::validate).
    pub scheduled_task_keep_awake: Duration,
    /// Preview-state reporter kill-switch
    /// (`ECHO_AGENT_WORKSPACE_PREVIEW_STATE_REPORTER_ENABLED`, default OFF).
    pub preview_state_reporter_enabled: bool,
    /// Poll cadence (`ECHO_AGENT_WORKSPACE_PREVIEW_STATE_POLL_INTERVAL_MS`);
    /// floored by [`validate`](Self::validate).
    pub preview_state_poll_interval: Duration,
    /// Preview-state long-poll hold (`ECHO_AGENT_WORKSPACE_PREVIEW_STATE_WAIT_SECS`):
    /// once the proxy's document carries a `generation`, the watcher holds
    /// `GET ?wait=<secs>&if_generation=<gen>` instead of fixed-interval
    /// polling. Zero (the default) disables long-polling; clamped to the
    /// proxy's own 15s hold ceiling by [`validate`](Self::validate).
    pub preview_state_wait: Duration,
    /// Preview-proxy discovery-scan cadence passthrough
    /// (`ECHO_AGENT_WORKSPACE_PREVIEW_DISCOVERY_REFRESH_MS`), forwarded by the
    /// supervisor as `--discovery-refresh-ms`. Zero (the default) omits the
    /// flag, leaving the proxy default; nonzero is clamped into [100ms, 10s]
    /// by [`validate`](Self::validate).
    pub preview_discovery_refresh: Duration,
    /// Proxy loopback control port from the `--preview-control-port` CLI flag
    /// (set by `workspace_server`, not env); `None` ⇒ the proxy default.
    pub preview_control_port: Option<u16>,
    /// True when this container booted via the sandbox restore path, which
    /// injects `ECHO_AGENT_SESSION_RESTORED=true`; a first boot never does.
    pub session_restored: bool,
    /// True when restore injects `ECHO_AGENT_REVIVE_SCRIPT_CONFIGURED=true` (launchable
    /// revive configured); unset on first boot and non-launchable restores.
    pub revive_script_configured: bool,
    /// True when restore injects `ECHO_AGENT_RESUME_NUDGE_DISABLED=true` (per-env
    /// `resume_nudge_disabled` sandbox config): the session-resumed nudge is
    /// suppressed at source for this boot.
    pub resume_nudge_disabled: bool,
    /// True when restore injects `ECHO_AGENT_COMPUTER_SESSION_RESUMED_EMIT=true` (sandbox
    /// `computer_session_resumed_emit` config field; default OFF). When false, the
    /// session-resumed nudge is suppressed at source.
    pub computer_session_resumed_emit: bool,
}

impl Default for StatusConfig {
    fn default() -> Self {
        Self {
            heartbeat: Duration::from_secs(DEFAULT_HEARTBEAT_SECS),
            keepalive: Duration::from_secs(DEFAULT_KEEPALIVE_SECS),
            ws_ping: Duration::from_secs(DEFAULT_WS_PING_SECS),
            ws_reconnect_backoff: None,
            ws_liveness_deadline: None,
            oidc_refresh: ProactiveRefreshConfig::default(),
            hub_warn_threshold: DEFAULT_HUB_WARN_THRESHOLD,
            hub_backoff_base: Duration::from_millis(DEFAULT_HUB_BACKOFF_BASE_MS),
            session_idle_prune: Duration::from_secs(DEFAULT_SESSION_IDLE_PRUNE_SECS),
            drain_timeout: Duration::from_secs(DEFAULT_DRAIN_TIMEOUT_SECS),
            agent_rpc_timeout: Duration::from_secs(DEFAULT_AGENT_RPC_TIMEOUT_SECS),
            agent_connect_timeout: Duration::from_secs(DEFAULT_AGENT_CONNECT_TIMEOUT_SECS),
            idle_ignores_background: false,
            preview_activity_window: Duration::from_millis(DEFAULT_PREVIEW_ACTIVITY_WINDOW_MS),
            preview_activity_scrape_interval: Duration::from_millis(
                DEFAULT_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS,
            ),
            rpc_activity_window: Duration::from_millis(DEFAULT_RPC_ACTIVITY_WINDOW_MS),
            presence_keepalive_enabled: false,
            presence_activity_window: Duration::from_millis(DEFAULT_PRESENCE_ACTIVITY_WINDOW_MS),
            scheduled_task_keep_awake: Duration::from_millis(DEFAULT_SCHEDULED_TASK_KEEP_AWAKE_MS),
            preview_state_reporter_enabled: false,
            preview_state_poll_interval: Duration::from_millis(
                DEFAULT_PREVIEW_STATE_POLL_INTERVAL_MS,
            ),
            preview_state_wait: Duration::from_secs(DEFAULT_PREVIEW_STATE_WAIT_SECS),
            preview_discovery_refresh: Duration::from_millis(DEFAULT_PREVIEW_DISCOVERY_REFRESH_MS),
            preview_control_port: None,
            session_restored: false,
            revive_script_configured: false,
            resume_nudge_disabled: false,
            computer_session_resumed_emit: false,
        }
    }
}

impl StatusConfig {
    /// Populate from `ECHO_AGENT_WORKSPACE_*`. Unset or unparseable vars fall
    /// back to the default with a `warn!`. Never fails.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        let (agent_rpc, agent_connect) = Self::agent_timeouts_from_env();
        let mut cfg = Self {
            heartbeat: secs_or("ECHO_AGENT_WORKSPACE_HEARTBEAT_SECS", defaults.heartbeat),
            keepalive: secs_or("ECHO_AGENT_WORKSPACE_KEEPALIVE_SECS", defaults.keepalive),
            ws_ping: secs_or("ECHO_AGENT_WORKSPACE_WS_PING_SECS", defaults.ws_ping),
            ws_reconnect_backoff: backoff_schedule_from_env(
                "ECHO_AGENT_WORKSPACE_WS_RECONNECT_BACKOFF_MS",
            ),
            ws_liveness_deadline: optional_secs("ECHO_AGENT_WORKSPACE_WS_LIVENESS_DEADLINE_SECS"),
            oidc_refresh: ProactiveRefreshConfig::from_env(),
            hub_warn_threshold: parse_or(
                "ECHO_AGENT_WORKSPACE_HUB_WARN_THRESHOLD",
                defaults.hub_warn_threshold,
            ),
            hub_backoff_base: ms_or(
                "ECHO_AGENT_WORKSPACE_HUB_BACKOFF_BASE_MS",
                defaults.hub_backoff_base,
            ),
            session_idle_prune: secs_or(
                "ECHO_AGENT_WORKSPACE_SESSION_IDLE_PRUNE_SECS",
                defaults.session_idle_prune,
            ),
            drain_timeout: secs_or(
                "ECHO_AGENT_WORKSPACE_DRAIN_TIMEOUT_SECS",
                defaults.drain_timeout,
            ),
            agent_rpc_timeout: agent_rpc,
            agent_connect_timeout: agent_connect,
            idle_ignores_background: parse_or(
                "ECHO_AGENT_WORKSPACE_IDLE_IGNORE_BACKGROUND_TASKS",
                defaults.idle_ignores_background,
            ),
            preview_activity_window: ms_or(
                "ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_WINDOW_MS",
                defaults.preview_activity_window,
            ),
            preview_activity_scrape_interval: ms_or(
                "ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS",
                defaults.preview_activity_scrape_interval,
            ),
            rpc_activity_window: ms_or(
                "ECHO_AGENT_WORKSPACE_RPC_ACTIVITY_WINDOW_MS",
                defaults.rpc_activity_window,
            ),
            presence_keepalive_enabled: parse_or(
                "ECHO_AGENT_WORKSPACE_PRESENCE_KEEPALIVE_ENABLED",
                defaults.presence_keepalive_enabled,
            ),
            presence_activity_window: ms_or(
                "ECHO_AGENT_WORKSPACE_PRESENCE_ACTIVITY_WINDOW_MS",
                defaults.presence_activity_window,
            ),
            scheduled_task_keep_awake: ms_or(
                "ECHO_AGENT_WORKSPACE_SCHEDULED_TASK_KEEP_AWAKE_MS",
                defaults.scheduled_task_keep_awake,
            ),
            preview_state_reporter_enabled: parse_or(
                "ECHO_AGENT_WORKSPACE_PREVIEW_STATE_REPORTER_ENABLED",
                defaults.preview_state_reporter_enabled,
            ),
            preview_state_poll_interval: ms_or(
                "ECHO_AGENT_WORKSPACE_PREVIEW_STATE_POLL_INTERVAL_MS",
                defaults.preview_state_poll_interval,
            ),
            preview_state_wait: secs_or(
                "ECHO_AGENT_WORKSPACE_PREVIEW_STATE_WAIT_SECS",
                defaults.preview_state_wait,
            ),
            preview_discovery_refresh: ms_or(
                "ECHO_AGENT_WORKSPACE_PREVIEW_DISCOVERY_REFRESH_MS",
                defaults.preview_discovery_refresh,
            ),
            preview_control_port: defaults.preview_control_port,
            session_restored: std::env::var("ECHO_AGENT_SESSION_RESTORED").as_deref() == Ok("true"),
            revive_script_configured: std::env::var("ECHO_AGENT_REVIVE_SCRIPT_CONFIGURED")
                .as_deref()
                == Ok("true"),
            resume_nudge_disabled: std::env::var("ECHO_AGENT_RESUME_NUDGE_DISABLED").as_deref()
                == Ok("true"),
            computer_session_resumed_emit: std::env::var(
                "ECHO_AGENT_COMPUTER_SESSION_RESUMED_EMIT",
            )
            .as_deref()
                == Ok("true"),
        };
        cfg.validate();
        cfg
    }

    /// Read only the agent gRPC `(request, connect)` timeouts from the
    /// environment, without parsing or validating the rest of the config.
    ///
    /// Used by the btrfs delegate's env-based construction path, which has no
    /// `StatusConfig` in scope; reading just these two vars avoids re-running
    /// [`validate`](Self::validate) (and its possible duplicate `warn!`).
    pub fn agent_timeouts_from_env() -> (Duration, Duration) {
        let defaults = Self::default();
        const RPC_VAR: &str = "ECHO_AGENT_WORKSPACE_AGENT_RPC_TIMEOUT_SECS";
        const CONNECT_VAR: &str = "ECHO_AGENT_WORKSPACE_AGENT_CONNECT_TIMEOUT_SECS";
        (
            nonzero_secs_or(
                RPC_VAR,
                parse_or(RPC_VAR, defaults.agent_rpc_timeout.as_secs()),
                defaults.agent_rpc_timeout,
            ),
            nonzero_secs_or(
                CONNECT_VAR,
                parse_or(CONNECT_VAR, defaults.agent_connect_timeout.as_secs()),
                defaults.agent_connect_timeout,
            ),
        )
    }

    /// The `ClientPresence` withhold window the tracker is wired with: zero
    /// unless the keepalive gate is on, so a window override alone can never
    /// enable the dark feature.
    pub fn effective_presence_activity_window(&self) -> Duration {
        if self.presence_keepalive_enabled {
            self.presence_activity_window
        } else {
            Duration::ZERO
        }
    }

    /// The `--discovery-refresh-ms` value the supervisor forwards to the
    /// proxy: `None` when the passthrough is off (zero), which omits the flag.
    pub fn preview_discovery_refresh_ms(&self) -> Option<u64> {
        let ms = self.preview_discovery_refresh.as_millis() as u64;
        (ms != 0).then_some(ms)
    }

    /// Warn on (and, where load-bearing, repair) inconsistent values.
    ///
    /// `keepalive` can't be validated against the server's idle window (unknown
    /// here), so it only warns. The preview scraper, however, must run strictly
    /// more often than the withhold window (else the withhold lapses between
    /// scrapes) and never at a zero interval (which would busy-loop it), so any
    /// misconfiguration is repaired into `1ms <= scrape < window`.
    fn validate(&mut self) {
        if self.keepalive <= self.heartbeat {
            tracing::warn!(
                keepalive = ?self.keepalive,
                heartbeat = ?self.heartbeat,
                "ECHO_AGENT_WORKSPACE keepalive <= heartbeat; transport may time out between heartbeats"
            );
        }
        let min_scrape = Duration::from_millis(MIN_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS);
        let window = self
            .preview_activity_window
            .max(Duration::from_millis(MIN_PREVIEW_ACTIVITY_WINDOW_MS));
        let scrape = self
            .preview_activity_scrape_interval
            .clamp(min_scrape, window - min_scrape);
        if window != self.preview_activity_window || scrape != self.preview_activity_scrape_interval
        {
            tracing::warn!(
                scrape_interval = ?self.preview_activity_scrape_interval,
                window = ?self.preview_activity_window,
                clamped_scrape = ?scrape,
                clamped_window = ?window,
                "ECHO_AGENT_WORKSPACE preview scrape interval/window out of range; clamped to 1ms <= scrape < window"
            );
            self.preview_activity_window = window;
            self.preview_activity_scrape_interval = scrape;
        }
        let rpc_cap = Duration::from_millis(MAX_RPC_ACTIVITY_WINDOW_MS);
        // Zero stays zero: it is the documented kill switch, not a hold.
        if self.rpc_activity_window > rpc_cap {
            tracing::warn!(
                window = ?self.rpc_activity_window,
                clamped_window = ?rpc_cap,
                "ECHO_AGENT_WORKSPACE rpc activity window above cap; clamped"
            );
            self.rpc_activity_window = rpc_cap;
        }
        let presence_cap = Duration::from_millis(MAX_PRESENCE_ACTIVITY_WINDOW_MS);
        if self.presence_activity_window > presence_cap {
            tracing::warn!(
                window = ?self.presence_activity_window,
                clamped_window = ?presence_cap,
                "ECHO_AGENT_WORKSPACE presence activity window above cap; clamped"
            );
            self.presence_activity_window = presence_cap;
        }
        let scheduled_cap = Duration::from_millis(MAX_SCHEDULED_TASK_KEEP_AWAKE_MS);
        if self.scheduled_task_keep_awake > scheduled_cap {
            tracing::warn!(
                window = ?self.scheduled_task_keep_awake,
                clamped_window = ?scheduled_cap,
                "ECHO_AGENT_WORKSPACE scheduled-task keep-awake window above cap; clamped"
            );
            self.scheduled_task_keep_awake = scheduled_cap;
        }
        let min_poll = Duration::from_millis(MIN_PREVIEW_STATE_POLL_INTERVAL_MS);
        if self.preview_state_poll_interval < min_poll {
            tracing::warn!(
                poll_interval = ?self.preview_state_poll_interval,
                floored_to = ?min_poll,
                "ECHO_AGENT_WORKSPACE preview-state poll interval below floor; floored"
            );
            self.preview_state_poll_interval = min_poll;
        }
        let wait_cap = Duration::from_secs(MAX_PREVIEW_STATE_WAIT_SECS);
        // Zero stays zero: it is the documented long-poll kill switch.
        if self.preview_state_wait > wait_cap {
            tracing::warn!(
                wait = ?self.preview_state_wait,
                clamped_wait = ?wait_cap,
                "ECHO_AGENT_WORKSPACE preview-state wait above the proxy's hold ceiling; clamped"
            );
            self.preview_state_wait = wait_cap;
        }
        // Zero stays zero: it means "omit the flag", not a cadence.
        if self.preview_discovery_refresh > Duration::ZERO {
            let refresh = self.preview_discovery_refresh.clamp(
                Duration::from_millis(MIN_PREVIEW_DISCOVERY_REFRESH_MS),
                Duration::from_millis(MAX_PREVIEW_DISCOVERY_REFRESH_MS),
            );
            if refresh != self.preview_discovery_refresh {
                tracing::warn!(
                    refresh = ?self.preview_discovery_refresh,
                    clamped_refresh = ?refresh,
                    "ECHO_AGENT_WORKSPACE preview discovery refresh out of range; clamped to 100ms..=10s"
                );
                self.preview_discovery_refresh = refresh;
            }
        }
        self.oidc_refresh.validate();
    }
}

/// Read `var` and parse it as `T`. Returns `default` when unset; warns and
/// returns `default` when present but unparseable.
fn parse_or<T: FromStr>(var: &str, default: T) -> T {
    match std::env::var(var) {
        Err(_) => default,
        Ok(raw) => match raw.parse::<T>() {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(var, value = %raw, "Unparseable ECHO_AGENT_WORKSPACE value; using default");
                default
            }
        },
    }
}

/// Parse `var` as a `u64` number of seconds into a [`Duration`].
fn secs_or(var: &str, default: Duration) -> Duration {
    Duration::from_secs(parse_or(var, default.as_secs()))
}

/// Parse `var` as an `f64`. Unset → `default`. Unparseable, non-finite, or
/// outside `in_range` → warn + `default`.
fn frac_or(var: &str, default: f64, in_range: impl Fn(f64) -> bool) -> f64 {
    match std::env::var(var) {
        Err(_) => default,
        Ok(raw) => match raw.parse::<f64>() {
            Ok(value) if value.is_finite() && in_range(value) => value,
            Ok(_) => {
                tracing::warn!(
                    var,
                    value = %raw,
                    "ECHO_AGENT_WORKSPACE fraction out of range; using default"
                );
                default
            }
            Err(_) => {
                tracing::warn!(
                    var,
                    value = %raw,
                    "Unparseable ECHO_AGENT_WORKSPACE value; using default"
                );
                default
            }
        },
    }
}

/// Parse `var` as optional seconds. Unset or unparseable → `None`.
fn optional_secs(var: &str) -> Option<Duration> {
    match std::env::var(var) {
        Err(_) => None,
        Ok(raw) => match raw.parse::<u64>() {
            Ok(secs) => Some(Duration::from_secs(secs)),
            Err(_) => {
                tracing::warn!(
                    var,
                    value = %raw,
                    "Unparseable ECHO_AGENT_WORKSPACE value; using default"
                );
                None
            }
        },
    }
}

/// Parse `var` as a `u64` number of milliseconds into a [`Duration`].
fn ms_or(var: &str, default: Duration) -> Duration {
    Duration::from_millis(parse_or(var, default.as_millis() as u64))
}

/// Convert a parsed seconds value into a [`Duration`], rejecting `0`. A zero
/// gRPC timeout makes every RPC fail immediately, so a configured `0` warns
/// and falls back to `default` instead of being applied verbatim.
fn nonzero_secs_or(var: &str, secs: u64, default: Duration) -> Duration {
    if secs == 0 {
        tracing::warn!(
            var,
            default = ?default,
            "ECHO_AGENT_WORKSPACE agent timeout of 0s is invalid; using default"
        );
        return default;
    }
    Duration::from_secs(secs)
}

/// Parse `var` as a comma-separated list of `u64` milliseconds into a reconnect
/// backoff schedule. Returns `None` (keep the SDK's built-in default schedule)
/// when the var is unset or yields no values, and warns + returns `None` when
/// any element fails to parse.
fn backoff_schedule_from_env(var: &str) -> Option<Vec<Duration>> {
    let raw = std::env::var(var).ok()?;
    let mut schedule = Vec::new();
    for part in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        match part.parse::<u64>() {
            Ok(ms) => schedule.push(Duration::from_millis(ms)),
            Err(_) => {
                tracing::warn!(
                    var,
                    value = %raw,
                    "Unparseable ECHO_AGENT_WORKSPACE backoff schedule; using SDK default"
                );
                return None;
            }
        }
    }
    (!schedule.is_empty()).then_some(schedule)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Crate-shared env lock: every test that mutates the process environment
    // holds it for its full duration. ONE lock for the whole crate because the
    // hazard is the global `environ` array under `unsafe set_var` (not the
    // variable's value), so even disjoint vars must serialize.
    use crate::ENV_TEST_LOCK as ENV_LOCK;

    #[test]
    fn defaults_match_documented_values() {
        let cfg = StatusConfig::default();
        assert_eq!(cfg.heartbeat, Duration::from_secs(30));
        assert_eq!(cfg.keepalive, Duration::from_secs(60));
        assert_eq!(cfg.ws_ping, Duration::from_secs(30));
        assert_eq!(cfg.ws_reconnect_backoff, None);
        assert_eq!(cfg.ws_liveness_deadline, None);
        assert_eq!(cfg.oidc_refresh, ProactiveRefreshConfig::default());
        assert!(!cfg.oidc_refresh.enabled);
        assert_eq!(cfg.oidc_refresh.fraction, 0.6);
        assert_eq!(cfg.oidc_refresh.jitter_fraction, 0.2);
        assert_eq!(cfg.oidc_refresh.safety_margin, Duration::from_secs(120));
        assert_eq!(
            cfg.oidc_refresh.min_refresh_interval,
            Duration::from_secs(60)
        );
        assert_eq!(cfg.hub_warn_threshold, 5);
        assert_eq!(cfg.hub_backoff_base, Duration::from_millis(100));
        assert_eq!(cfg.session_idle_prune, Duration::from_secs(1800));
        assert_eq!(cfg.drain_timeout, Duration::from_secs(30));
        assert_eq!(cfg.agent_rpc_timeout, Duration::from_secs(30));
        assert_eq!(cfg.agent_connect_timeout, Duration::from_secs(5));
        assert!(!cfg.idle_ignores_background);
        assert_eq!(cfg.preview_activity_window, Duration::from_secs(60));
        assert_eq!(
            cfg.preview_activity_scrape_interval,
            Duration::from_secs(10)
        );
        assert_eq!(cfg.rpc_activity_window, Duration::from_secs(60));
        assert!(!cfg.presence_keepalive_enabled);
        assert_eq!(cfg.presence_activity_window, Duration::from_secs(90));
        assert_eq!(
            cfg.scheduled_task_keep_awake,
            Duration::from_secs(13 * 3600)
        );
        assert!(!cfg.preview_state_reporter_enabled);
        assert_eq!(cfg.preview_state_poll_interval, Duration::from_secs(5));
        assert_eq!(cfg.preview_state_wait, Duration::ZERO);
        assert_eq!(cfg.preview_discovery_refresh, Duration::ZERO);
        assert_eq!(cfg.preview_discovery_refresh_ms(), None);
        assert!(!cfg.session_restored);
        assert!(!cfg.revive_script_configured);
        assert!(!cfg.resume_nudge_disabled);
        assert!(!cfg.computer_session_resumed_emit);
    }

    #[test]
    fn preview_state_reporter_env_parses_and_floors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let enabled_var = "ECHO_AGENT_WORKSPACE_PREVIEW_STATE_REPORTER_ENABLED";
        let interval_var = "ECHO_AGENT_WORKSPACE_PREVIEW_STATE_POLL_INTERVAL_MS";

        unsafe { std::env::set_var(enabled_var, "true") };
        unsafe { std::env::set_var(interval_var, "0") };
        let cfg = StatusConfig::from_env();
        assert!(cfg.preview_state_reporter_enabled);
        assert_eq!(
            cfg.preview_state_poll_interval,
            Duration::from_millis(MIN_PREVIEW_STATE_POLL_INTERVAL_MS),
            "zero interval must be floored, not busy-loop"
        );

        unsafe { std::env::set_var(enabled_var, "yes") };
        unsafe { std::env::set_var(interval_var, "2500") };
        let cfg = StatusConfig::from_env();
        assert!(
            !cfg.preview_state_reporter_enabled,
            "non-bool spelling falls back to the OFF default"
        );
        assert_eq!(cfg.preview_state_poll_interval, Duration::from_millis(2500));

        unsafe { std::env::remove_var(enabled_var) };
        unsafe { std::env::remove_var(interval_var) };
        let cfg = StatusConfig::from_env();
        assert!(!cfg.preview_state_reporter_enabled);
    }

    #[test]
    fn preview_state_wait_env_parses_and_clamps_to_the_proxy_hold_ceiling() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_PREVIEW_STATE_WAIT_SECS";

        unsafe { std::env::remove_var(var) };
        assert_eq!(
            StatusConfig::from_env().preview_state_wait,
            Duration::ZERO,
            "unset ⇒ long-poll disabled"
        );

        unsafe { std::env::set_var(var, "10") };
        assert_eq!(
            StatusConfig::from_env().preview_state_wait,
            Duration::from_secs(10)
        );

        unsafe { std::env::set_var(var, "60") };
        assert_eq!(
            StatusConfig::from_env().preview_state_wait,
            Duration::from_secs(MAX_PREVIEW_STATE_WAIT_SECS),
            "the proxy clamps ?wait to 15s; a larger value only inflates the client timeout"
        );

        unsafe { std::env::set_var(var, "not-a-number") };
        assert_eq!(
            StatusConfig::from_env().preview_state_wait,
            Duration::ZERO,
            "unparseable falls back to the disabled default"
        );

        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn preview_discovery_refresh_env_parses_floors_and_clamps() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_PREVIEW_DISCOVERY_REFRESH_MS";

        unsafe { std::env::remove_var(var) };
        assert_eq!(
            StatusConfig::from_env().preview_discovery_refresh_ms(),
            None,
            "unset ⇒ the supervisor omits --discovery-refresh-ms"
        );

        unsafe { std::env::set_var(var, "0") };
        assert_eq!(
            StatusConfig::from_env().preview_discovery_refresh_ms(),
            None,
            "explicit zero is the documented omit switch"
        );

        unsafe { std::env::set_var(var, "500") };
        assert_eq!(
            StatusConfig::from_env().preview_discovery_refresh_ms(),
            Some(500)
        );

        unsafe { std::env::set_var(var, "50") };
        assert_eq!(
            StatusConfig::from_env().preview_discovery_refresh_ms(),
            Some(MIN_PREVIEW_DISCOVERY_REFRESH_MS),
            "sub-floor values would near-busy-loop the proxy's /proc scan"
        );

        unsafe { std::env::set_var(var, "60000") };
        assert_eq!(
            StatusConfig::from_env().preview_discovery_refresh_ms(),
            Some(MAX_PREVIEW_DISCOVERY_REFRESH_MS),
            "a seconds-for-ms typo is repaired to the ceiling"
        );

        unsafe { std::env::set_var(var, "abc") };
        assert_eq!(
            StatusConfig::from_env().preview_discovery_refresh_ms(),
            None,
            "unparseable falls back to the omit default"
        );

        unsafe { std::env::remove_var(var) };
    }

    /// `parse_or` returns the default when the variable is unset. Uses a
    /// uniquely-named var so it never collides with other tests' env writes.
    #[test]
    fn parse_or_unset_returns_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_PARSE_OR_UNSET";
        unsafe { std::env::remove_var(var) };
        assert_eq!(parse_or::<u32>(var, 5), 5);
    }

    #[test]
    fn parse_or_valid_parses() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_PARSE_OR_VALID";
        unsafe { std::env::set_var(var, "42") };
        assert_eq!(parse_or::<u32>(var, 5), 42);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn parse_or_invalid_falls_back_without_panic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_PARSE_OR_INVALID";
        unsafe { std::env::set_var(var, "not-a-number") };
        assert_eq!(parse_or::<u32>(var, 5), 5);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn secs_or_parses_into_duration() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_SECS_OR_VALID";
        unsafe { std::env::set_var(var, "120") };
        assert_eq!(
            secs_or(var, Duration::from_secs(30)),
            Duration::from_secs(120)
        );
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn secs_or_unset_returns_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_SECS_OR_UNSET";
        unsafe { std::env::remove_var(var) };
        assert_eq!(
            secs_or(var, Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn secs_or_invalid_falls_back_without_panic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_SECS_OR_INVALID";
        unsafe { std::env::set_var(var, "12.5") };
        assert_eq!(
            secs_or(var, Duration::from_secs(30)),
            Duration::from_secs(30)
        );
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn ms_or_parses_into_duration() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_MS_OR_VALID";
        unsafe { std::env::set_var(var, "250") };
        assert_eq!(
            ms_or(var, Duration::from_millis(100)),
            Duration::from_millis(250)
        );
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn ms_or_invalid_falls_back_without_panic() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_MS_OR_INVALID";
        unsafe { std::env::set_var(var, "abc") };
        assert_eq!(
            ms_or(var, Duration::from_millis(100)),
            Duration::from_millis(100)
        );
        unsafe { std::env::remove_var(var) };
    }

    /// With none of the `ECHO_AGENT_WORKSPACE_*` vars set, `from_env` reproduces
    /// `StatusConfig::default()` field-for-field.
    ///
    /// This is the one test that touches the real (non-`_TEST_`-prefixed)
    /// var names: it `remove_var`s all of them before reading them. If a future
    /// test sets these shared names, run them serialized to avoid a race.
    #[test]
    fn from_env_clean_matches_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for var in [
            "ECHO_AGENT_WORKSPACE_HEARTBEAT_SECS",
            "ECHO_AGENT_WORKSPACE_KEEPALIVE_SECS",
            "ECHO_AGENT_WORKSPACE_WS_PING_SECS",
            "ECHO_AGENT_WORKSPACE_WS_RECONNECT_BACKOFF_MS",
            "ECHO_AGENT_WORKSPACE_WS_LIVENESS_DEADLINE_SECS",
            "ECHO_AGENT_WORKSPACE_OIDC_PROACTIVE_REFRESH_ENABLED",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_FRACTION",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_JITTER_FRACTION",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_SAFETY_MARGIN_SECS",
            "ECHO_AGENT_WORKSPACE_OIDC_MIN_REFRESH_INTERVAL_SECS",
            "ECHO_AGENT_WORKSPACE_HUB_WARN_THRESHOLD",
            "ECHO_AGENT_WORKSPACE_HUB_BACKOFF_BASE_MS",
            "ECHO_AGENT_WORKSPACE_SESSION_IDLE_PRUNE_SECS",
            "ECHO_AGENT_WORKSPACE_DRAIN_TIMEOUT_SECS",
            "ECHO_AGENT_WORKSPACE_AGENT_RPC_TIMEOUT_SECS",
            "ECHO_AGENT_WORKSPACE_AGENT_CONNECT_TIMEOUT_SECS",
            "ECHO_AGENT_WORKSPACE_IDLE_IGNORE_BACKGROUND_TASKS",
            "ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_WINDOW_MS",
            "ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS",
            "ECHO_AGENT_WORKSPACE_RPC_ACTIVITY_WINDOW_MS",
            "ECHO_AGENT_WORKSPACE_PRESENCE_KEEPALIVE_ENABLED",
            "ECHO_AGENT_WORKSPACE_PRESENCE_ACTIVITY_WINDOW_MS",
            "ECHO_AGENT_WORKSPACE_PREVIEW_STATE_WAIT_SECS",
            "ECHO_AGENT_WORKSPACE_PREVIEW_DISCOVERY_REFRESH_MS",
            "ECHO_AGENT_SESSION_RESTORED",
            "ECHO_AGENT_REVIVE_SCRIPT_CONFIGURED",
            "ECHO_AGENT_RESUME_NUDGE_DISABLED",
            "ECHO_AGENT_COMPUTER_SESSION_RESUMED_EMIT",
        ] {
            unsafe { std::env::remove_var(var) };
        }
        let cfg = StatusConfig::from_env();
        let default = StatusConfig::default();
        assert_eq!(cfg.heartbeat, default.heartbeat);
        assert_eq!(cfg.keepalive, default.keepalive);
        assert_eq!(cfg.ws_ping, default.ws_ping);
        assert_eq!(cfg.ws_reconnect_backoff, default.ws_reconnect_backoff);
        assert_eq!(cfg.ws_liveness_deadline, default.ws_liveness_deadline);
        assert_eq!(cfg.oidc_refresh, default.oidc_refresh);
        assert_eq!(cfg.hub_warn_threshold, default.hub_warn_threshold);
        assert_eq!(cfg.hub_backoff_base, default.hub_backoff_base);
        assert_eq!(cfg.session_idle_prune, default.session_idle_prune);
        assert_eq!(cfg.drain_timeout, default.drain_timeout);
        assert_eq!(cfg.agent_rpc_timeout, default.agent_rpc_timeout);
        assert_eq!(cfg.agent_connect_timeout, default.agent_connect_timeout);
        assert_eq!(cfg.idle_ignores_background, default.idle_ignores_background);
        assert_eq!(cfg.preview_activity_window, default.preview_activity_window);
        assert_eq!(
            cfg.preview_activity_scrape_interval,
            default.preview_activity_scrape_interval
        );
        assert_eq!(cfg.rpc_activity_window, default.rpc_activity_window);
        assert_eq!(
            cfg.presence_keepalive_enabled,
            default.presence_keepalive_enabled
        );
        assert_eq!(
            cfg.presence_activity_window,
            default.presence_activity_window
        );
        assert_eq!(cfg.preview_state_wait, default.preview_state_wait);
        assert_eq!(
            cfg.preview_discovery_refresh,
            default.preview_discovery_refresh
        );
        assert_eq!(cfg.session_restored, default.session_restored);
        assert_eq!(
            cfg.revive_script_configured,
            default.revive_script_configured
        );
        assert_eq!(cfg.resume_nudge_disabled, default.resume_nudge_disabled);
        assert_eq!(
            cfg.computer_session_resumed_emit,
            default.computer_session_resumed_emit
        );
    }

    #[test]
    fn from_env_reads_session_restored_true_only() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ECHO_AGENT_SESSION_RESTORED", "true") };
        let restored = StatusConfig::from_env().session_restored;
        unsafe { std::env::set_var("ECHO_AGENT_SESSION_RESTORED", "1") };
        let non_canonical = StatusConfig::from_env().session_restored;
        unsafe { std::env::remove_var("ECHO_AGENT_SESSION_RESTORED") };
        assert!(restored);
        assert!(!non_canonical);
    }

    #[test]
    fn from_env_reads_revive_script_configured_true_only() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ECHO_AGENT_REVIVE_SCRIPT_CONFIGURED", "true") };
        let configured = StatusConfig::from_env().revive_script_configured;
        unsafe { std::env::set_var("ECHO_AGENT_REVIVE_SCRIPT_CONFIGURED", "1") };
        let non_canonical = StatusConfig::from_env().revive_script_configured;
        unsafe { std::env::remove_var("ECHO_AGENT_REVIVE_SCRIPT_CONFIGURED") };
        assert!(configured);
        assert!(!non_canonical);
    }

    #[test]
    fn from_env_reads_resume_nudge_disabled_true_only() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ECHO_AGENT_RESUME_NUDGE_DISABLED", "true") };
        let disabled = StatusConfig::from_env().resume_nudge_disabled;
        unsafe { std::env::set_var("ECHO_AGENT_RESUME_NUDGE_DISABLED", "1") };
        let non_canonical = StatusConfig::from_env().resume_nudge_disabled;
        unsafe { std::env::remove_var("ECHO_AGENT_RESUME_NUDGE_DISABLED") };
        assert!(disabled);
        assert!(!non_canonical);
    }

    #[test]
    fn from_env_reads_computer_session_resumed_emit_true_only() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ECHO_AGENT_COMPUTER_SESSION_RESUMED_EMIT", "true") };
        let enabled = StatusConfig::from_env().computer_session_resumed_emit;
        unsafe { std::env::set_var("ECHO_AGENT_COMPUTER_SESSION_RESUMED_EMIT", "1") };
        let non_canonical = StatusConfig::from_env().computer_session_resumed_emit;
        unsafe { std::env::remove_var("ECHO_AGENT_COMPUTER_SESSION_RESUMED_EMIT") };
        assert!(enabled);
        assert!(!non_canonical);
    }

    #[test]
    fn from_env_reads_idle_ignore_background_true() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_IDLE_IGNORE_BACKGROUND_TASKS", "true") };
        let cfg = StatusConfig::from_env();
        unsafe { std::env::remove_var("ECHO_AGENT_WORKSPACE_IDLE_IGNORE_BACKGROUND_TASKS") };
        assert!(cfg.idle_ignores_background);
    }

    #[test]
    fn from_env_reads_preview_activity_window() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_WINDOW_MS", "120000") };
        let cfg = StatusConfig::from_env();
        unsafe { std::env::remove_var("ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_WINDOW_MS") };
        assert_eq!(cfg.preview_activity_window, Duration::from_millis(120_000));
    }

    #[test]
    fn from_env_reads_preview_activity_scrape_interval() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var(
                "ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS",
                "5000",
            )
        };
        let cfg = StatusConfig::from_env();
        unsafe { std::env::remove_var("ECHO_AGENT_WORKSPACE_PREVIEW_ACTIVITY_SCRAPE_INTERVAL_MS") };
        assert_eq!(
            cfg.preview_activity_scrape_interval,
            Duration::from_millis(5_000)
        );
    }

    /// `validate` must not panic regardless of the relative ordering of
    /// `keepalive` and `heartbeat`.
    #[test]
    fn validate_does_not_panic_when_keepalive_le_heartbeat() {
        let mut cfg = StatusConfig {
            keepalive: Duration::from_secs(10),
            heartbeat: Duration::from_secs(30),
            ..StatusConfig::default()
        };
        cfg.validate();
    }

    #[test]
    fn validate_clamps_preview_scrape_into_valid_range() {
        for (window_ms, scrape_ms, exp_window_ms, exp_scrape_ms) in [
            (1_000u64, 4_000u64, 1_000u64, 999u64),
            (1_000, 1_000, 1_000, 999),
            (1_000, 200, 1_000, 200),
            (60_000, 0, 60_000, 1),
            (0, 0, 2, 1),
            (1, 5, 2, 1),
        ] {
            let mut cfg = StatusConfig {
                preview_activity_window: Duration::from_millis(window_ms),
                preview_activity_scrape_interval: Duration::from_millis(scrape_ms),
                ..StatusConfig::default()
            };
            cfg.validate();
            assert_eq!(
                cfg.preview_activity_window,
                Duration::from_millis(exp_window_ms)
            );
            assert_eq!(
                cfg.preview_activity_scrape_interval,
                Duration::from_millis(exp_scrape_ms)
            );
            assert!(cfg.preview_activity_scrape_interval >= Duration::from_millis(1));
            assert!(cfg.preview_activity_scrape_interval < cfg.preview_activity_window);
        }
    }

    /// Values past the cap are repaired; `0` — the kill switch — never is.
    #[test]
    fn validate_clamps_rpc_activity_window_but_spares_the_kill_switch() {
        for (window_ms, expected_ms) in [(0u64, 0u64), (60_000, 60_000), (86_400_000, 600_000)] {
            let mut cfg = StatusConfig {
                rpc_activity_window: Duration::from_millis(window_ms),
                ..StatusConfig::default()
            };
            cfg.validate();
            assert_eq!(
                cfg.rpc_activity_window,
                Duration::from_millis(expected_ms),
                "window {window_ms}ms"
            );
        }
    }

    #[test]
    fn from_env_reads_rpc_activity_window() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_RPC_ACTIVITY_WINDOW_MS", "30000") };
        let cfg = StatusConfig::from_env();
        unsafe { std::env::remove_var("ECHO_AGENT_WORKSPACE_RPC_ACTIVITY_WINDOW_MS") };
        assert_eq!(cfg.rpc_activity_window, Duration::from_millis(30_000));
    }

    #[test]
    fn from_env_reads_presence_activity_window() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_PRESENCE_ACTIVITY_WINDOW_MS", "45000") };
        let cfg = StatusConfig::from_env();
        unsafe { std::env::remove_var("ECHO_AGENT_WORKSPACE_PRESENCE_ACTIVITY_WINDOW_MS") };
        assert_eq!(cfg.presence_activity_window, Duration::from_millis(45_000));
    }

    #[test]
    fn presence_keepalive_env_gates_the_effective_window() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let enabled_var = "ECHO_AGENT_WORKSPACE_PRESENCE_KEEPALIVE_ENABLED";
        let window_var = "ECHO_AGENT_WORKSPACE_PRESENCE_ACTIVITY_WINDOW_MS";

        unsafe { std::env::remove_var(enabled_var) };
        unsafe { std::env::set_var(window_var, "45000") };
        let cfg = StatusConfig::from_env();
        assert!(!cfg.presence_keepalive_enabled);
        assert_eq!(cfg.effective_presence_activity_window(), Duration::ZERO);

        unsafe { std::env::set_var(enabled_var, "true") };
        let cfg = StatusConfig::from_env();
        assert!(cfg.presence_keepalive_enabled);
        assert_eq!(
            cfg.effective_presence_activity_window(),
            Duration::from_millis(45_000)
        );

        unsafe { std::env::set_var(enabled_var, "yes") };
        let cfg = StatusConfig::from_env();
        assert!(
            !cfg.presence_keepalive_enabled,
            "non-bool spelling falls back to the OFF default"
        );

        unsafe { std::env::remove_var(enabled_var) };
        unsafe { std::env::remove_var(window_var) };
    }

    #[test]
    fn validate_clamps_scheduled_task_keep_awake_but_spares_the_kill_switch() {
        for (window_ms, expected_ms) in [
            (0u64, 0u64),
            (46_800_000, 46_800_000),
            (30 * 24 * 3_600_000, 7 * 24 * 3_600_000),
        ] {
            let mut cfg = StatusConfig {
                scheduled_task_keep_awake: Duration::from_millis(window_ms),
                ..StatusConfig::default()
            };

            cfg.validate();

            assert_eq!(
                cfg.scheduled_task_keep_awake,
                Duration::from_millis(expected_ms),
                "window {window_ms}ms"
            );
        }
    }

    #[test]
    fn validate_clamps_presence_activity_window_but_spares_the_kill_switch() {
        for (window_ms, expected_ms) in [(0u64, 0u64), (90_000, 90_000), (86_400_000, 600_000)] {
            let mut cfg = StatusConfig {
                presence_activity_window: Duration::from_millis(window_ms),
                ..StatusConfig::default()
            };
            cfg.validate();
            assert_eq!(
                cfg.presence_activity_window,
                Duration::from_millis(expected_ms),
                "window {window_ms}ms"
            );
        }
    }

    /// A configured agent timeout of `0` seconds is invalid (it would make
    /// every gRPC call fail immediately), so `nonzero_secs_or` falls back to
    /// the supplied default instead of returning `Duration::ZERO`.
    #[test]
    fn nonzero_secs_or_zero_falls_back_to_default() {
        assert_eq!(
            nonzero_secs_or(
                "ECHO_AGENT_WORKSPACE_AGENT_RPC_TIMEOUT_SECS",
                0,
                Duration::from_secs(30)
            ),
            Duration::from_secs(30)
        );
        assert_eq!(
            nonzero_secs_or(
                "ECHO_AGENT_WORKSPACE_AGENT_CONNECT_TIMEOUT_SECS",
                0,
                Duration::from_secs(5)
            ),
            Duration::from_secs(5)
        );
    }

    /// A positive value is honored verbatim.
    #[test]
    fn nonzero_secs_or_positive_is_passed_through() {
        assert_eq!(
            nonzero_secs_or(
                "ECHO_AGENT_WORKSPACE_AGENT_RPC_TIMEOUT_SECS",
                12,
                Duration::from_secs(30)
            ),
            Duration::from_secs(12)
        );
    }

    /// An unset backoff var leaves the schedule unconfigured (`None`), so the
    /// SDK keeps its built-in default.
    #[test]
    fn backoff_schedule_unset_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_BACKOFF_UNSET";
        unsafe { std::env::remove_var(var) };
        assert_eq!(backoff_schedule_from_env(var), None);
    }

    /// A valid comma-separated list parses into millisecond `Duration`s in
    /// order, tolerating surrounding whitespace.
    #[test]
    fn backoff_schedule_valid_list_parses_in_order() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_BACKOFF_VALID";
        unsafe { std::env::set_var(var, "100, 200,500,1000") };
        assert_eq!(
            backoff_schedule_from_env(var),
            Some(vec![
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(500),
                Duration::from_millis(1000),
            ])
        );
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn frac_or_unset_returns_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_FRAC_OR_UNSET";
        unsafe { std::env::remove_var(var) };
        assert_eq!(frac_or(var, 0.6, |v| v > 0.0 && v < 1.0), 0.6);
    }

    #[test]
    fn frac_or_valid_parses() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_FRAC_OR_VALID";
        unsafe { std::env::set_var(var, "0.75") };
        assert_eq!(frac_or(var, 0.6, |v| v > 0.0 && v < 1.0), 0.75);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn frac_or_garbage_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_FRAC_OR_GARBAGE";
        unsafe { std::env::set_var(var, "not-a-fraction") };
        assert_eq!(frac_or(var, 0.6, |v| v > 0.0 && v < 1.0), 0.6);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn frac_or_out_of_range_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_FRAC_OR_RANGE";
        unsafe { std::env::set_var(var, "0") };
        assert_eq!(frac_or(var, 0.6, |v| v > 0.0 && v < 1.0), 0.6);
        unsafe { std::env::set_var(var, "1") };
        assert_eq!(frac_or(var, 0.6, |v| v > 0.0 && v < 1.0), 0.6);
        unsafe { std::env::set_var(var, "1.5") };
        assert_eq!(frac_or(var, 0.6, |v| v > 0.0 && v < 1.0), 0.6);
        unsafe { std::env::set_var(var, "-0.1") };
        assert_eq!(frac_or(var, 0.2, |v| (0.0..=0.5).contains(&v)), 0.2);
        unsafe { std::env::set_var(var, "0.6") };
        assert_eq!(frac_or(var, 0.2, |v| (0.0..=0.5).contains(&v)), 0.2);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn oidc_refresh_env_parses_and_defaults() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        for var in [
            "ECHO_AGENT_WORKSPACE_OIDC_PROACTIVE_REFRESH_ENABLED",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_FRACTION",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_JITTER_FRACTION",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_SAFETY_MARGIN_SECS",
            "ECHO_AGENT_WORKSPACE_OIDC_MIN_REFRESH_INTERVAL_SECS",
        ] {
            unsafe { std::env::remove_var(var) };
        }
        let cfg = ProactiveRefreshConfig::from_env();
        assert_eq!(cfg, ProactiveRefreshConfig::default());
        assert!(!cfg.enabled);

        unsafe {
            std::env::set_var(
                "ECHO_AGENT_WORKSPACE_OIDC_PROACTIVE_REFRESH_ENABLED",
                "true",
            )
        };
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_OIDC_REFRESH_FRACTION", "0.7") };
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_OIDC_REFRESH_JITTER_FRACTION", "0.1") };
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_OIDC_REFRESH_SAFETY_MARGIN_SECS", "90") };
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_OIDC_MIN_REFRESH_INTERVAL_SECS", "30") };
        let cfg = ProactiveRefreshConfig::from_env();
        assert!(cfg.enabled);
        assert_eq!(cfg.fraction, 0.7);
        assert_eq!(cfg.jitter_fraction, 0.1);
        assert_eq!(cfg.safety_margin, Duration::from_secs(90));
        assert_eq!(cfg.min_refresh_interval, Duration::from_secs(30));

        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_OIDC_REFRESH_FRACTION", "nope") };
        unsafe { std::env::set_var("ECHO_AGENT_WORKSPACE_OIDC_REFRESH_JITTER_FRACTION", "9") };
        let cfg = ProactiveRefreshConfig::from_env();
        assert_eq!(cfg.fraction, 0.6);
        assert_eq!(cfg.jitter_fraction, 0.2);

        for var in [
            "ECHO_AGENT_WORKSPACE_OIDC_PROACTIVE_REFRESH_ENABLED",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_FRACTION",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_JITTER_FRACTION",
            "ECHO_AGENT_WORKSPACE_OIDC_REFRESH_SAFETY_MARGIN_SECS",
            "ECHO_AGENT_WORKSPACE_OIDC_MIN_REFRESH_INTERVAL_SECS",
        ] {
            unsafe { std::env::remove_var(var) };
        }
    }

    #[test]
    fn oidc_min_refresh_interval_zero_is_floored() {
        let mut cfg = ProactiveRefreshConfig {
            min_refresh_interval: Duration::ZERO,
            ..ProactiveRefreshConfig::default()
        };
        cfg.validate();
        assert_eq!(cfg.min_refresh_interval, MIN_OIDC_MIN_REFRESH_INTERVAL);
        assert!(cfg.min_refresh_interval < cfg.safety_margin);
    }

    #[test]
    fn oidc_min_refresh_interval_at_or_above_safety_is_repaired() {
        let mut cfg = ProactiveRefreshConfig {
            safety_margin: Duration::from_secs(30),
            min_refresh_interval: Duration::from_secs(60),
            ..ProactiveRefreshConfig::default()
        };
        cfg.validate();
        assert!(cfg.min_refresh_interval < cfg.safety_margin);
        assert_eq!(cfg.min_refresh_interval, Duration::from_secs(29));

        let mut tiny = ProactiveRefreshConfig {
            safety_margin: Duration::ZERO,
            min_refresh_interval: Duration::from_secs(5),
            ..ProactiveRefreshConfig::default()
        };
        tiny.validate();
        assert!(tiny.min_refresh_interval < tiny.safety_margin);
        assert_eq!(tiny.min_refresh_interval, Duration::from_secs(5));
        assert_eq!(tiny.safety_margin, Duration::from_secs(6));
    }

    #[test]
    fn oidc_min_at_duration_cap_with_tiny_safety_stays_capped() {
        let mut cfg = ProactiveRefreshConfig {
            safety_margin: Duration::from_secs(1),
            min_refresh_interval: MAX_OIDC_DURATION,
            ..ProactiveRefreshConfig::default()
        };
        cfg.validate();
        assert!(cfg.min_refresh_interval < cfg.safety_margin);
        assert_eq!(cfg.safety_margin, MAX_OIDC_DURATION);
        assert_eq!(
            cfg.min_refresh_interval,
            MAX_OIDC_DURATION - Duration::from_secs(1)
        );
    }

    #[test]
    fn oidc_fractions_invalid_direct_construction_falls_back_to_defaults() {
        for fraction in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            1.0,
            1.5,
            -0.1,
        ] {
            let mut cfg = ProactiveRefreshConfig {
                fraction,
                ..ProactiveRefreshConfig::default()
            };
            cfg.validate();
            assert_eq!(
                cfg.fraction, DEFAULT_OIDC_REFRESH_FRACTION,
                "fraction={fraction}"
            );
        }
        for jitter in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 0.6, 1.0] {
            let mut cfg = ProactiveRefreshConfig {
                jitter_fraction: jitter,
                ..ProactiveRefreshConfig::default()
            };
            cfg.validate();
            assert_eq!(
                cfg.jitter_fraction, DEFAULT_OIDC_REFRESH_JITTER_FRACTION,
                "jitter_fraction={jitter}"
            );
        }

        let mut ok = ProactiveRefreshConfig {
            fraction: 0.7,
            jitter_fraction: 0.1,
            ..ProactiveRefreshConfig::default()
        };
        ok.validate();
        assert_eq!(ok.fraction, 0.7);
        assert_eq!(ok.jitter_fraction, 0.1);
    }

    #[test]
    fn oidc_min_refresh_interval_zero_env_is_floored() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_OIDC_MIN_REFRESH_INTERVAL_SECS";
        unsafe { std::env::set_var(var, "0") };
        let cfg = ProactiveRefreshConfig::from_env();
        assert_eq!(cfg.min_refresh_interval, MIN_OIDC_MIN_REFRESH_INTERVAL);
        unsafe { std::env::remove_var(var) };
    }

    #[test]
    fn ws_liveness_deadline_env_parses() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_WS_LIVENESS_DEADLINE_SECS";

        unsafe { std::env::remove_var(var) };
        assert_eq!(StatusConfig::from_env().ws_liveness_deadline, None);

        unsafe { std::env::set_var(var, "90") };
        assert_eq!(
            StatusConfig::from_env().ws_liveness_deadline,
            Some(Duration::from_secs(90))
        );

        unsafe { std::env::set_var(var, "not-a-number") };
        assert_eq!(StatusConfig::from_env().ws_liveness_deadline, None);

        unsafe { std::env::remove_var(var) };
    }

    /// A malformed element makes the whole schedule fall back to `None` (and
    /// warns) rather than silently dropping entries.
    #[test]
    fn backoff_schedule_malformed_returns_none() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let var = "ECHO_AGENT_WORKSPACE_TEST_BACKOFF_MALFORMED";
        unsafe { std::env::set_var(var, "100,not-a-number,500") };
        assert_eq!(backoff_schedule_from_env(var), None);
        unsafe { std::env::remove_var(var) };
    }
}
