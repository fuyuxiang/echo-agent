//! One-child supervisor for the in-sandbox preview-proxy.
//!
//! After the workspace-server self-daemonizes (see [`crate::daemonize`]) it
//! spawns the unchanged `/usr/local/bin/echo-agent-preview-proxy` binary as a
//! child process and supervises exactly that one child: fork/exec → `wait` →
//! restart-on-exit with capped backoff that resets after a healthy run.
//!
//! Two properties depend on *where* this runs:
//! - The child is spawned only from [`supervise_preview`], which the bin invokes
//!   **after** daemonize. The child therefore inherits the daemon's new
//!   session/pgid and escapes the launcher's process-group reap — one daemonize
//!   protects both processes.
//! - `PR_SET_PDEATHSIG(SIGKILL)` binds the child's lifetime to the
//!   workspace-server so a WS crash cannot orphan the proxy holding the ports.
//!   PDEATHSIG keys off the *spawning thread*, so the fork→exec race (WS dies
//!   before the child arms `prctl`) is closed by re-checking `getppid()` in
//!   `pre_exec`, and the supervise task is spawned on tokio's long-lived
//!   `multi_thread` workers to avoid a spurious worker-thread-death kill.

use std::fs::{self, File};
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use prometheus::{IntCounterVec, register_int_counter_vec};
use tokio::sync::watch;

/// Absolute path of the preview-proxy binary the supervisor execs.
pub const PREVIEW_PROXY_BIN_PATH: &str = "/usr/local/bin/echo-agent-preview-proxy";

/// WS-owned, per-restart-truncated log capturing the proxy's stdout+stderr. A
/// sibling of `WORKSPACE_SERVER_LOG_PATH` on the snapshot-excluded `/var/tmp`
/// overlay (NOT `/tmp`, which is the in-namespace tmpfs rebind), so it persists
/// and is retrievable via the sandbox session log retrieval path.
pub const PREVIEW_PROXY_LOG_PATH: &str = "/var/tmp/workspace-server/tmp/preview-proxy.log";

/// A child that ran at least this long is treated as a healthy run, resetting
/// the restart backoff.
pub const PREVIEW_PROXY_HEALTHY_RUN_SECS: u64 = 30;

/// First/base restart delay; doubled on each consecutive unhealthy restart.
pub const PREVIEW_PROXY_RESTART_BACKOFF_BASE_SECS: u64 = 1;

/// Ceiling on the restart backoff so a crash-loop pins at this interval rather
/// than growing unbounded.
pub const PREVIEW_PROXY_RESTART_BACKOFF_CAP_SECS: u64 = 30;

/// `echo_agent_workspace_preview_proxy_restart_total{reason}` — (re)start events the
/// in-sandbox supervisor emits, by reason; tracks preview-proxy restart pressure.
static PREVIEW_PROXY_RESTART_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "echo_agent_workspace_preview_proxy_restart_total",
        "preview-proxy (re)start events emitted by the in-sandbox supervisor, by reason",
        &["reason"]
    )
    .unwrap()
});

/// Reason label for the restart metric.
enum RestartReason {
    Exit,
    SpawnError,
}

impl RestartReason {
    fn as_str(&self) -> &'static str {
        match self {
            RestartReason::Exit => "exit",
            RestartReason::SpawnError => "spawn_error",
        }
    }
}

fn record_restart(reason: RestartReason) {
    PREVIEW_PROXY_RESTART_TOTAL
        .with_label_values(&[reason.as_str()])
        .inc();
}

/// Access policy forwarded to the proxy's `--visibility`. Mirrors the proxy's
/// own enum values (`owner` | `public`) without depending on its crate, and
/// constrains the workspace-server CLI so a bad value fails fast at startup
/// rather than crash-looping the proxy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum PreviewVisibility {
    Owner,
    Public,
}

impl PreviewVisibility {
    fn as_str(self) -> &'static str {
        match self {
            PreviewVisibility::Owner => "owner",
            PreviewVisibility::Public => "public",
        }
    }
}

/// Supervisor config forwarded to the proxy child. `Option` fields are omitted
/// from argv when absent (the proxy applies its own defaults); per-session
/// secrets stay in the inherited env, never argv.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewArgs {
    /// Gate: the supervisor is started only when this is true. Not forwarded —
    /// the proxy has no such flag.
    pub enabled: bool,
    /// → proxy `--preview-port`.
    pub port: Option<u16>,
    /// → proxy `--control-port`.
    pub control_port: Option<u16>,
    /// → proxy `--visibility` (`owner` | `public`).
    pub visibility: Option<PreviewVisibility>,
    /// → proxy `--instance-suffix`.
    pub instance_suffix: Option<String>,
    /// → proxy `--auth-redirect` (URL the unauthenticated handshake redirects
    /// to). Without it the owner gate denies instead of redirecting.
    pub auth_redirect: Option<String>,
    /// → proxy `--allow-public` (a bare flag, emitted only when true).
    pub allow_public: bool,
    /// → proxy `--workspace-server-port`.
    pub workspace_server_port: Option<u16>,
    /// → proxy `--discovery-refresh-ms` (candidate-scan cadence). `None` omits
    /// the flag — a proxy binary predating it rejects the unknown flag and
    /// would crash-loop — so the env stays unset until the proxy release rolls.
    pub discovery_refresh_ms: Option<u64>,
    /// `current_dir` for the spawned child. Not forwarded as an arg.
    pub workspace_dir: PathBuf,
}

impl PreviewArgs {
    /// Map the forwarded fields to the proxy's exact CLI flag names (see
    /// `echo-agent-preview-proxy/src/cli.rs`). Absent options and a false
    /// `allow_public` contribute nothing; the `enabled` gate is never emitted.
    pub fn to_argv(&self) -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(port) = self.port {
            argv.push("--preview-port".to_owned());
            argv.push(port.to_string());
        }
        if let Some(port) = self.control_port {
            argv.push("--control-port".to_owned());
            argv.push(port.to_string());
        }
        if let Some(visibility) = self.visibility {
            argv.push("--visibility".to_owned());
            argv.push(visibility.as_str().to_owned());
        }
        if let Some(suffix) = &self.instance_suffix {
            argv.push("--instance-suffix".to_owned());
            argv.push(suffix.clone());
        }
        if let Some(redirect) = &self.auth_redirect {
            argv.push("--auth-redirect".to_owned());
            argv.push(redirect.clone());
        }
        if self.allow_public {
            argv.push("--allow-public".to_owned());
        }
        if let Some(port) = self.workspace_server_port {
            argv.push("--workspace-server-port".to_owned());
            argv.push(port.to_string());
        }
        if let Some(refresh_ms) = self.discovery_refresh_ms {
            argv.push("--discovery-refresh-ms".to_owned());
            argv.push(refresh_ms.to_string());
        }
        argv
    }
}

/// Exponential restart backoff with a hard ceiling. The step counter is the
/// number of consecutive unhealthy restarts; a healthy run resets it.
#[derive(Clone, Copy, Debug)]
struct BackoffPolicy {
    base: Duration,
    cap: Duration,
}

impl BackoffPolicy {
    fn new(base: Duration, cap: Duration) -> Self {
        Self { base, cap }
    }

    /// `base * 2^step`, saturating to `cap` (overflow ⇒ cap).
    fn delay(self, step: u32) -> Duration {
        let factor = 2u32.saturating_pow(step);
        self.base
            .checked_mul(factor)
            .unwrap_or(self.cap)
            .min(self.cap)
    }
}

/// Delay before the next spawn and the next step counter: a healthy run resets to
/// the base delay; an unhealthy run (or spawn failure) advances the backoff.
fn next_step(policy: BackoffPolicy, healthy: bool, step: u32) -> (Duration, u32) {
    if healthy {
        (policy.delay(0), 0)
    } else {
        (policy.delay(step), step.saturating_add(1))
    }
}

/// Healthy once the run reached `healthy_run` (inclusive); resets the backoff.
fn is_healthy(elapsed: Duration, healthy_run: Duration) -> bool {
    elapsed >= healthy_run
}

/// Open the WS-owned proxy log, truncating it on every (re)start so a crash-loop
/// pinned at the backoff cap cannot grow it unbounded. Reuses the daemon file
/// options (`O_NOFOLLOW` + mode `0600` on Unix) for the same symlink/permission
/// defense as the workspace-server log.
fn open_truncated_log(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    crate::daemonize::daemon_file_options()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Async-signal-safe write of a fixed `oom_score_adj` for use inside `pre_exec`.
/// Always returns `Ok(())`: missing procfs and short/failed writes must not
/// block proxy spawn. On a short or failed write after a successful open,
/// logs a static warning to stderr (async-signal-safe).
#[cfg(target_os = "linux")]
fn write_oom_score_adj_raw(value: &'static [u8]) -> io::Result<()> {
    const PATH: &[u8] = b"/proc/self/oom_score_adj\0";
    const WARN: &[u8] = b"WARN preview-proxy: failed to write oom_score_adj\n";
    // SAFETY: PATH/WARN are static NUL-terminated / static bytes; value is 'static.
    unsafe {
        let fd = libc::open(PATH.as_ptr().cast(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd < 0 {
            // Missing procfs must not block spawn.
            return Ok(());
        }
        let n = libc::write(fd, value.as_ptr().cast(), value.len());
        libc::close(fd);
        if n < 0 || n as usize != value.len() {
            let _ = libc::write(2, WARN.as_ptr().cast(), WARN.len());
        }
    }
    Ok(())
}

/// Build the unspawned proxy command. Secrets (`ECHO_AGENT_SERVER_KEY` /
/// `ECHO_AGENT_SESSION_ID`) reach the proxy by env inheritance — never argv.
fn build_preview_command(cfg: &PreviewArgs) -> io::Result<tokio::process::Command> {
    use std::process::Stdio;

    let log = open_truncated_log(Path::new(PREVIEW_PROXY_LOG_PATH))?;
    let log_err = log.try_clone()?;

    let mut cmd = std::process::Command::new(PREVIEW_PROXY_BIN_PATH);
    cmd.args(cfg.to_argv())
        .current_dir(&cfg.workspace_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    // Linux-only: PDEATHSIG (prctl) does not exist on macOS/other unixes, so this
    // parent-death binding is gated to Linux. The proxy simply runs without the
    // binding elsewhere.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;

        // Raw pre_exec, NOT echo_agent_tty_utils::detach_command: the proxy must stay in
        // the workspace-server's session/pgid to share its reap-escape, so the
        // setsid that detach_command performs would be actively wrong here (and
        // the daemonized server owns no controlling TTY, so the detach rationale
        // does not apply). This raw path is also deliberately exempt from the
        // shared child OOM reset: the proxy sets -500 (or resets to 0) below.
        //
        // PDEATHSIG keys off the spawning thread, so capture our PID to also
        // close the fork→exec race in the child.
        let parent_pid = std::process::id();
        // Read env pre-fork: env access is not async-signal-safe inside pre_exec.
        // Armed when always-on protect succeeds and/or `--oom-protect` forces it.
        let oom_protect = std::env::var_os(echo_agent_tty_utils::RESET_CHILD_OOM_ENV).is_some();
        // SAFETY: the closure runs in the forked child between fork and exec, so
        // it calls only async-signal-safe libc functions (`prctl`, `getppid`,
        // `open`/`write`/`close`, `_exit`) and touches no allocation/locks/Rust
        // runtime state. Its error path returns `io::Error::last_os_error()`,
        // which only wraps the raw errno (no allocation). Never use
        // `io::Error::new`/`other` here, as they allocate.
        unsafe {
            cmd.pre_exec(move || {
                // Bind the proxy's lifetime to the workspace-server: a WS crash
                // makes the kernel SIGKILL the proxy so it can't orphan and hold
                // the preview/control ports. This binding survives the proxy's
                // own execve only because that binary is non-setuid and carries
                // no file capabilities (the kernel clears PDEATHSIG across a
                // privileged exec).
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong) == -1 {
                    return Err(io::Error::last_os_error());
                }
                // If the WS already exited (PDEATHSIG won't fire), bail before
                // exec rather than orphan.
                if libc::getppid() as u32 != parent_pid {
                    libc::_exit(0);
                }
                // OOM score: when protect env is on, raise inherited -900 to -500
                // (no CAP_SYS_RESOURCE). Reset-to-0 then lower would fail in nested
                // userns. When env is off, reset to 0 so we never inherit -900.
                if oom_protect {
                    // "-500\n" matches PREVIEW_PROXY_OOM_SCORE_ADJ; static for
                    // async-signal-safety (no formatting/allocation in pre_exec).
                    write_oom_score_adj_raw(b"-500\n")?;
                } else {
                    echo_agent_tty_utils::reset_oom_score_adj()?;
                }
                Ok(())
            });
        }
    }

    Ok(tokio::process::Command::from(cmd))
}

/// Supervise the preview-proxy child until `shutdown` flips. Spawn this **after**
/// daemonize (see module docs) so the child inherits the new session/pgid.
pub async fn supervise_preview(cfg: PreviewArgs, shutdown: watch::Receiver<bool>) {
    tracing::info!(
        bin = PREVIEW_PROXY_BIN_PATH,
        log = PREVIEW_PROXY_LOG_PATH,
        workspace_dir = %cfg.workspace_dir.display(),
        "starting preview-proxy supervisor",
    );
    let policy = BackoffPolicy::new(
        Duration::from_secs(PREVIEW_PROXY_RESTART_BACKOFF_BASE_SECS),
        Duration::from_secs(PREVIEW_PROXY_RESTART_BACKOFF_CAP_SECS),
    );
    let healthy_run = Duration::from_secs(PREVIEW_PROXY_HEALTHY_RUN_SECS);
    supervise_loop(
        move || build_preview_command(&cfg),
        policy,
        healthy_run,
        shutdown,
    )
    .await;
}

/// Core supervise loop, generic over the command factory so tests can drive a
/// fake child without the real proxy binary.
async fn supervise_loop<F>(
    mut make_command: F,
    policy: BackoffPolicy,
    healthy_run: Duration,
    mut shutdown: watch::Receiver<bool>,
) where
    F: FnMut() -> io::Result<tokio::process::Command>,
{
    let mut step = 0u32;
    while !*shutdown.borrow() {
        let started = Instant::now();
        #[allow(clippy::disallowed_methods)] // supervised preview child, killed on drop
        let spawned = make_command().and_then(|mut cmd| cmd.kill_on_drop(true).spawn());
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                // Spawn/log-open failure must back off and retry — never drop the task.
                tracing::error!(error = %e, bin = PREVIEW_PROXY_BIN_PATH, "preview-proxy spawn failed; backing off");
                record_restart(RestartReason::SpawnError);
                let (delay, next) = next_step(policy, false, step);
                step = next;
                if sleep_or_shutdown(delay, &mut shutdown).await {
                    return;
                }
                continue;
            }
        };
        tracing::info!(pid = child.id(), "preview-proxy started");

        tokio::select! {
            status = child.wait() => {
                // A shutdown racing the exit must not be counted as a restart.
                if *shutdown.borrow() {
                    return;
                }
                let ran = started.elapsed();
                let healthy = is_healthy(ran, healthy_run);
                record_restart(RestartReason::Exit);
                let (delay, next) = next_step(policy, healthy, step);
                step = next;
                tracing::warn!(
                    ?status,
                    healthy,
                    ran_secs = ran.as_secs(),
                    backoff_secs = delay.as_secs(),
                    "preview-proxy exited; restarting",
                );
                if sleep_or_shutdown(delay, &mut shutdown).await {
                    return;
                }
            }
            // SIGKILL on teardown: preview is best-effort and the container is
            // going away.
            _ = shutdown.changed() => {
                let _ = child.kill().await;
                tracing::info!("supervisor received shutdown; killed preview-proxy");
                return;
            }
        }
    }
}

/// Sleep for `delay`, returning early with `true` if `shutdown` flips (or all
/// senders drop) during the wait — the caller then tears down and returns.
async fn sleep_or_shutdown(delay: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        _ = shutdown.changed() => true,
    }
}

// ── Preview-activity scraper ───────────────────────────────────────────────
//
// Polls the proxy's loopback `/__control/activity` and reports through
// `PreviewActivitySink` so in-sandbox preview traffic withholds idle.

/// Proxy control path exposing the last-activity stamp (mirrors
/// `echo-agent-preview-proxy`'s `/__control/activity` route).
const PREVIEW_ACTIVITY_PATH: &str = "/__control/activity";

/// Effective proxy `--control-port` when the supervisor didn't pin one (mirrors
/// the default in `echo-agent-preview-proxy/src/cli.rs`).
pub const DEFAULT_PREVIEW_CONTROL_PORT: u16 = 6015;

/// Where the scraper reports what it read off the proxy.
///
/// The workspace-server implements this over its `ActivityTracker`, whose
/// idle-withhold accounting is the only consumer today. It is a trait rather
/// than the concrete tracker so this crate stays free of `echo-agent-workspace`:
/// the daemon machinery is used by the server binary alone, and a dependency
/// on the library would put it back into every downstream build.
pub trait PreviewActivitySink: Send + Sync + 'static {
    /// A request was routed through the proxy since the previous scrape.
    fn note_preview_routed_activity(&self);
    /// The proxy's generic activity stamp advanced without a routed request
    /// (a status poll, a handshake).
    fn note_preview_status_activity(&self);
    /// Absolute mirror of the proxy's attached-client counters, republished on
    /// every scrape that returned trustworthy data.
    fn set_preview_attached(&self, ws_tunnels_open: u64, routed_in_flight: u64);
    /// How long a stamp keeps withholding idle. Doubles as the staleness grace
    /// for the mirrored attached counters, so both holds expire on one clock.
    fn preview_activity_window_ms(&self) -> u64;
}

/// Per-scrape budget. The endpoint is loopback and trivial, so a small bound is
/// ample and a wedged proxy can't stall the scraper.
const PREVIEW_ACTIVITY_SCRAPE_TIMEOUT: Duration = Duration::from_secs(2);

/// The proxy's loopback activity URL for `control_port`. Shared by the scrape
/// loop and its tests so a URL-shape change can't drift between them.
fn activity_url(control_port: u16) -> String {
    format!(
        "http://{}:{control_port}{PREVIEW_ACTIVITY_PATH}",
        Ipv4Addr::LOCALHOST
    )
}

/// One scrape of the proxy's activity endpoint. `last_activity_ms` is the only
/// required field; the rest default to zero, so a workspace-server running
/// ahead of the proxy binary degrades to the old behaviour rather than failing.
#[derive(Debug, Default, PartialEq, Eq, Clone, Copy, serde::Deserialize)]
struct ActivitySample {
    last_activity_ms: u64,
    #[serde(default)]
    last_routed_ms: u64,
    #[serde(default)]
    ws_tunnels_open: u64,
    #[serde(default)]
    routed_requests_in_flight: u64,
}

/// Classified result of one scrape, so a missing proxy (quiet no-op) is never
/// confused with a genuine error response or with real activity.
#[derive(Debug, PartialEq, Eq)]
enum ScrapeOutcome {
    /// The proxy answered with a parseable activity sample.
    Stamp(ActivitySample),
    /// The proxy isn't reachable (connection refused / not up yet): quiet no-op.
    Absent,
    /// The proxy answered but the response was unusable (error status / bad body).
    BadResponse,
}

/// Parse an activity body; `None` for a malformed body or a missing/non-integer
/// `last_activity_ms`. Unknown fields are ignored so the proxy can add more.
fn parse_activity_body(body: &str) -> Option<ActivitySample> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    // Required, and must be an integer.
    value.get("last_activity_ms")?.as_u64()?;
    serde_json::from_value(value).ok()
}

/// Classify a completed response by status + body (transport failures are
/// classified by the caller). Only a 2xx carrying a parseable stamp is data.
fn classify_activity_response(status: u16, body: &str) -> ScrapeOutcome {
    if !(200..300).contains(&status) {
        return ScrapeOutcome::BadResponse;
    }
    match parse_activity_body(body) {
        Some(sample) => ScrapeOutcome::Stamp(sample),
        None => ScrapeOutcome::BadResponse,
    }
}

/// Whether a scraped stamp is strictly newer than the last seen — i.e. the proxy
/// recorded preview traffic since. A non-increasing value (incl. a proxy
/// restart-to-zero) is not an advance.
fn preview_activity_advanced(last_seen: u64, current: u64) -> bool {
    current > last_seen
}

/// One scrape, classified fail-open: a connect/timeout failure (proxy absent or
/// still starting) is `Absent` (quiet), kept distinct from a genuine error
/// response so neither is ever read as activity.
async fn scrape_activity(client: &reqwest::Client, url: &str) -> ScrapeOutcome {
    match client.get(url).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            match resp.text().await {
                Ok(body) => classify_activity_response(status, &body),
                Err(_) => ScrapeOutcome::BadResponse,
            }
        }
        Err(e) if e.is_connect() || e.is_timeout() => ScrapeOutcome::Absent,
        Err(_) => ScrapeOutcome::BadResponse,
    }
}

/// Clear the mirrored attached-client counters once we have been without
/// trustworthy data for `grace`. Starts the clock on the first bad scrape.
fn clear_attached_if_stale(
    tracker: &dyn PreviewActivitySink,
    stale_since: &mut Option<Instant>,
    grace: Duration,
) {
    let since = *stale_since.get_or_insert_with(Instant::now);
    if since.elapsed() >= grace {
        tracker.set_preview_attached(0, 0);
    }
}

/// Poll the proxy's loopback activity endpoint until `shutdown` flips, feeding
/// the sink on each advance. Spawn after the sink's tracker exists (post
/// hub-connect), gated on preview being enabled. `control_port` is the proxy's
/// loopback control port; `None` falls back to [`DEFAULT_PREVIEW_CONTROL_PORT`].
/// `scrape_interval` comes from `StatusConfig` (kept strictly below the withhold
/// window by `StatusConfig::validate`).
pub async fn supervise_preview_activity(
    control_port: Option<u16>,
    tracker: Arc<dyn PreviewActivitySink>,
    scrape_interval: Duration,
    shutdown: watch::Receiver<bool>,
) {
    scrape_activity_loop(
        control_port.unwrap_or(DEFAULT_PREVIEW_CONTROL_PORT),
        tracker,
        scrape_interval,
        shutdown,
    )
    .await;
}

/// Core scrape loop, parameterized by the scrape interval so tests drive it fast.
async fn scrape_activity_loop(
    control_port: u16,
    tracker: Arc<dyn PreviewActivitySink>,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
) {
    if *shutdown.borrow() {
        return;
    }
    let url = activity_url(control_port);
    // A fixed loopback control endpoint never redirects, so a 3xx is anomalous —
    // don't follow it; it classifies as `BadResponse`.
    #[allow(clippy::disallowed_methods)] // localhost preview server; TLS policy N/A
    let client = match reqwest::Client::builder()
        .timeout(PREVIEW_ACTIVITY_SCRAPE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!(error = %e, "preview-activity scraper: HTTP client build failed; disabled");
            return;
        }
    };
    tracing::info!(%url, "starting preview-activity scraper");

    // `None` until the first successful scrape establishes a baseline. Baselining
    // (rather than starting at 0) avoids a spurious withhold when a workspace-server
    // restart meets a proxy whose stamp is already non-zero but stale.
    let mut last_seen: Option<ActivitySample> = None;
    // When we last had trustworthy attached-client data. The counters are the
    // ONLY hold a WS-only client has — a tunnel writes no stamps — so one bad
    // scrape must not clear them; but unlike the stamps they do not decay, so a
    // sustained loss must, or a proxy that dies mid-tunnel holds the sandbox to
    // the TTL. `Absent` and `BadResponse` both count as loss: one cannot reach
    // the proxy, the other cannot understand it. The threshold is the stamp
    // window, so both hold mechanisms expire on the same clock.
    let attached_grace = Duration::from_millis(tracker.preview_activity_window_ms());
    let mut attached_stale_since: Option<Instant> = None;
    loop {
        if sleep_or_shutdown(interval, &mut shutdown).await {
            return;
        }
        match scrape_activity(&client, &url).await {
            ScrapeOutcome::Stamp(current) => {
                if let Some(prev) = last_seen {
                    // The routed stamp is a subset of the generic one, so check
                    // it first and attribute only the remainder to a poll —
                    // otherwise one routed request would report as both.
                    if preview_activity_advanced(prev.last_routed_ms, current.last_routed_ms) {
                        tracker.note_preview_routed_activity();
                    } else if preview_activity_advanced(
                        prev.last_activity_ms,
                        current.last_activity_ms,
                    ) {
                        tracker.note_preview_status_activity();
                    }
                }
                // Absolute counters, republished every tick: a mirror of the
                // proxy's state, not an edge.
                tracker.set_preview_attached(
                    current.ws_tunnels_open,
                    current.routed_requests_in_flight,
                );
                last_seen = Some(current);
                attached_stale_since = None;
            }
            // Proxy absent (preview disabled / starting / restarting): leave
            // the stamps, and age the attached counters out. See above.
            ScrapeOutcome::Absent => {
                clear_attached_if_stale(&*tracker, &mut attached_stale_since, attached_grace);
            }
            // Answering, but unusably. Same staleness clock: an error status or
            // an unparseable body tells us nothing about attached clients, and
            // leaving them untouched would let a persistently broken proxy hold
            // the withhold open forever.
            ScrapeOutcome::BadResponse => {
                tracing::debug!(%url, "preview-activity scrape returned an unusable response");
                clear_attached_if_stale(&*tracker, &mut attached_stale_since, attached_grace);
            }
        }
    }
}

// ── Preview-metrics scraper ────────────────────────────────────────────────

const PREVIEW_METRICS_PATH: &str = "/__control/metrics";
const PREVIEW_METRICS_PREFIX: &str = "preview_proxy_";
const PREVIEW_METRICS_SCRAPE_INTERVAL: Duration = Duration::from_secs(60);

fn metrics_url(control_port: u16) -> String {
    format!(
        "http://{}:{control_port}{PREVIEW_METRICS_PATH}",
        Ipv4Addr::LOCALHOST
    )
}

/// Scrapes the proxy's loopback-only metrics and donates them via the hub pump.
pub async fn supervise_preview_metrics(control_port: Option<u16>, shutdown: watch::Receiver<bool>) {
    scrape_metrics_loop(
        control_port.unwrap_or(DEFAULT_PREVIEW_CONTROL_PORT),
        PREVIEW_METRICS_SCRAPE_INTERVAL,
        shutdown,
        |body| {
            if let Some(sink) = echo_agent_computer_hub_sdk::metric_donate::active_metrics_sink() {
                sink.export_text_exposition(body, PREVIEW_METRICS_PREFIX);
            }
        },
    )
    .await;
}

async fn scrape_metrics_loop(
    control_port: u16,
    interval: Duration,
    mut shutdown: watch::Receiver<bool>,
    mut donate: impl FnMut(&str),
) {
    if *shutdown.borrow() {
        return;
    }
    let url = metrics_url(control_port);
    #[allow(clippy::disallowed_methods)] // localhost preview server; TLS policy N/A
    let client = match reqwest::Client::builder()
        .timeout(PREVIEW_ACTIVITY_SCRAPE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
    {
        Ok(client) => client,
        Err(e) => {
            tracing::warn!(error = %e, "preview-metrics scraper: HTTP client build failed; disabled");
            return;
        }
    };
    tracing::info!(%url, "starting preview-metrics scraper");

    // Scrape-first: a short-lived sandbox must not exit with zero samples.
    loop {
        match client.get(&url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.text().await {
                Ok(body) => donate(&body),
                Err(e) => {
                    tracing::debug!(%url, error = %e, "preview-metrics scrape body read failed");
                }
            },
            Ok(resp) => {
                tracing::debug!(%url, status = resp.status().as_u16(), "preview-metrics scrape returned an error status");
            }
            // Proxy absent (disabled / starting / restarting): quiet no-op.
            Err(e) if e.is_connect() || e.is_timeout() => {}
            Err(e) => {
                tracing::debug!(%url, error = %e, "preview-metrics scrape failed");
            }
        }
        if sleep_or_shutdown(interval, &mut shutdown).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::SeqCst};

    use tokio::net::TcpSocket;

    use super::*;

    /// Stand-in for the workspace-server's `ActivityTracker`. Records exactly
    /// what the scraper reported, so these tests assert the scraper's contract
    /// without linking the workspace library; the tracker's own idle-withhold
    /// accounting is covered by `echo_agent_workspace::activity`, and the wiring
    /// between the two by the `echo-agent-workspace-server` binary's own tests.
    struct TestSink {
        window_ms: u64,
        routed_notes: AtomicU64,
        status_notes: AtomicU64,
        ws_tunnels_open: AtomicU64,
        routed_in_flight: AtomicU64,
    }

    impl TestSink {
        fn with_window_ms(window_ms: u64) -> Arc<Self> {
            Arc::new(Self {
                window_ms,
                routed_notes: AtomicU64::new(0),
                status_notes: AtomicU64::new(0),
                ws_tunnels_open: AtomicU64::new(0),
                routed_in_flight: AtomicU64::new(0),
            })
        }

        /// Default window, matching `ActivityTracker::new()`'s posture for the
        /// tests that never exercise the staleness grace.
        fn new() -> Arc<Self> {
            Self::with_window_ms(60_000)
        }

        /// Any advance the scraper attributed, routed or status. Non-zero is the
        /// sink-side equivalent of "the tracker is withholding idle".
        fn activity_notes(&self) -> u64 {
            self.routed_notes.load(SeqCst) + self.status_notes.load(SeqCst)
        }

        /// The mirrored attached-client counters, as last republished.
        fn attached(&self) -> (u64, u64) {
            (
                self.ws_tunnels_open.load(SeqCst),
                self.routed_in_flight.load(SeqCst),
            )
        }

        fn ws_tunnels_open(&self) -> u64 {
            self.ws_tunnels_open.load(SeqCst)
        }
    }

    /// Erase a `TestSink` to the trait object the scrape loop takes.
    fn sink_of(sink: &Arc<TestSink>) -> Arc<dyn PreviewActivitySink> {
        // Annotated so `Arc::clone` resolves against `TestSink`, not the
        // trait-object return type; the unsizing happens on the return.
        let concrete: Arc<TestSink> = Arc::clone(sink);
        concrete
    }

    impl PreviewActivitySink for TestSink {
        fn note_preview_routed_activity(&self) {
            self.routed_notes.fetch_add(1, SeqCst);
        }

        fn note_preview_status_activity(&self) {
            self.status_notes.fetch_add(1, SeqCst);
        }

        fn set_preview_attached(&self, ws_tunnels_open: u64, routed_in_flight: u64) {
            self.ws_tunnels_open.store(ws_tunnels_open, SeqCst);
            self.routed_in_flight.store(routed_in_flight, SeqCst);
        }

        fn preview_activity_window_ms(&self) -> u64 {
            self.window_ms
        }
    }

    fn sample_cfg() -> PreviewArgs {
        PreviewArgs {
            enabled: true,
            port: Some(6014),
            control_port: Some(6015),
            visibility: Some(PreviewVisibility::Public),
            instance_suffix: Some(".inst.example".to_owned()),
            auth_redirect: Some("https://cloud.echo-agent.invalid/preview-auth".to_owned()),
            allow_public: true,
            workspace_server_port: Some(8470),
            discovery_refresh_ms: Some(250),
            workspace_dir: PathBuf::from("/workspace"),
        }
    }

    #[test]
    fn to_argv_maps_every_flag_to_the_proxy_cli_names() {
        // Flag names must match echo-agent-preview-proxy/src/cli.rs exactly.
        assert_eq!(
            sample_cfg().to_argv(),
            vec![
                "--preview-port",
                "6014",
                "--control-port",
                "6015",
                "--visibility",
                "public",
                "--instance-suffix",
                ".inst.example",
                "--auth-redirect",
                "https://cloud.echo-agent.invalid/preview-auth",
                "--allow-public",
                "--workspace-server-port",
                "8470",
                "--discovery-refresh-ms",
                "250",
            ],
        );
    }

    #[test]
    fn to_argv_omits_absent_options_and_false_allow_public() {
        let cfg = PreviewArgs {
            enabled: true,
            port: None,
            control_port: None,
            visibility: None,
            instance_suffix: None,
            auth_redirect: None,
            allow_public: false,
            workspace_server_port: None,
            discovery_refresh_ms: None,
            workspace_dir: PathBuf::from("/workspace"),
        };
        assert!(
            cfg.to_argv().is_empty(),
            "absent options + false allow_public ⇒ the proxy uses its own \
             defaults; --discovery-refresh-ms in particular must be omitted"
        );
    }

    #[test]
    fn to_argv_never_emits_the_enabled_gate() {
        // `enabled` gates whether the supervisor runs; it is not a proxy flag and
        // must never leak into argv, regardless of its value.
        let mut cfg = sample_cfg();
        cfg.enabled = true;
        let enabled_argv = cfg.to_argv();
        cfg.enabled = false;
        let disabled_argv = cfg.to_argv();
        assert_eq!(enabled_argv, disabled_argv, "the gate must not affect argv");
        assert!(!enabled_argv.iter().any(|a| a.contains("enabled")));
    }

    #[test]
    fn to_argv_lowers_owner_visibility() {
        // The common (default) case: `Owner` lowers to the proxy's `owner` value.
        let mut cfg = sample_cfg();
        cfg.visibility = Some(PreviewVisibility::Owner);
        let argv = cfg.to_argv();
        let i = argv
            .iter()
            .position(|a| a == "--visibility")
            .expect("--visibility present");
        assert_eq!(argv[i + 1], "owner");
    }

    #[test]
    fn backoff_doubles_caps_at_30s_and_resets_after_a_healthy_run() {
        let policy = BackoffPolicy::new(Duration::from_secs(1), Duration::from_secs(30));

        // Consecutive unhealthy restarts: 1, 2, 4, 8, 16, then pinned at the 30s
        // cap (32 → 30, 64 → 30).
        let mut step = 0u32;
        for want in [1u64, 2, 4, 8, 16, 30, 30] {
            let (delay, next) = next_step(policy, false, step);
            assert_eq!(delay, Duration::from_secs(want), "step {step}");
            step = next;
        }

        // A healthy run resets to the base delay and zeroes the step…
        let (delay, next) = next_step(policy, true, step);
        assert_eq!(delay, Duration::from_secs(1));
        assert_eq!(next, 0);

        // …and the exponential progression starts over from the base.
        let (delay, next) = next_step(policy, false, next);
        assert_eq!(delay, Duration::from_secs(1));
        assert_eq!(next, 1);
    }

    #[test]
    fn backoff_does_not_overflow_at_extreme_steps() {
        let policy = BackoffPolicy::new(Duration::from_secs(1), Duration::from_secs(30));
        // Huge step must saturate to the cap, never panic/overflow.
        assert_eq!(policy.delay(u32::MAX), Duration::from_secs(30));
        let (delay, next) = next_step(policy, false, u32::MAX);
        assert_eq!(delay, Duration::from_secs(30));
        assert_eq!(next, u32::MAX, "step saturates instead of wrapping");
    }

    #[test]
    fn is_healthy_boundary_is_inclusive() {
        let threshold = Duration::from_secs(PREVIEW_PROXY_HEALTHY_RUN_SECS);
        assert!(!is_healthy(threshold - Duration::from_millis(1), threshold));
        assert!(is_healthy(threshold, threshold), "boundary is healthy");
        assert!(is_healthy(threshold + Duration::from_millis(1), threshold));
    }

    #[test]
    fn open_truncated_log_truncates_prior_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview-proxy.log");
        std::fs::write(&path, "stale output from a prior crash-looping run\n").unwrap();

        let _file = open_truncated_log(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            0,
            "the log must be truncated to 0 on each (re)start"
        );
    }

    #[test]
    fn preview_proxy_oom_adj_is_between_user_and_workspace() {
        // Static pre_exec write uses b"-500\n"; keep the constant in lockstep.
        assert_eq!(crate::daemonize::PREVIEW_PROXY_OOM_SCORE_ADJ, -500);
        const {
            assert!(
                crate::daemonize::PREVIEW_PROXY_OOM_SCORE_ADJ
                    > crate::daemonize::WORKSPACE_SERVER_OOM_SCORE_ADJ
            );
            assert!(crate::daemonize::PREVIEW_PROXY_OOM_SCORE_ADJ < 0);
        }
    }

    /// The proxy uses a raw `pre_exec`, not `detach_pre_exec_hook`, so the
    /// shared child-OOM reset never runs on it. Documented by construction:
    /// `build_preview_command` is the only spawn path and is the raw path.
    #[cfg(target_os = "linux")]
    #[test]
    fn write_oom_score_adj_raw_lowers_when_permitted() {
        let _guard = crate::daemonize::TEST_OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let own = std::fs::read_to_string("/proc/self/oom_score_adj").expect("read");
        let restore = own.trim().to_owned();
        // Best-effort lower via the same path as pre_exec.
        write_oom_score_adj_raw(b"-500\n").unwrap();
        let after = std::fs::read_to_string("/proc/self/oom_score_adj").unwrap();
        if after.trim() == "-500" {
            // success under CAP_SYS_RESOURCE
        } else {
            // without CAP the open/write is best-effort Ok and score stays put
            assert_eq!(after.trim(), restore);
        }
        let _ = std::fs::write("/proc/self/oom_score_adj", format!("{restore}\n"));
    }

    /// Nested userns path: parent is already at -900; raise to -500 needs no CAP.
    /// Reset-then-lower would fail there (cannot re-lower from 0 without CAP).
    #[cfg(target_os = "linux")]
    #[test]
    fn raise_preview_score_from_workspace_score() {
        let _guard = crate::daemonize::TEST_OOM_SCORE_ADJ_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let own = std::fs::read_to_string("/proc/self/oom_score_adj").expect("read");
        let restore = own.trim().to_owned();
        // Plant workspace score first (needs CAP); skip when unavailable.
        if crate::daemonize::set_oom_score_adj(crate::daemonize::WORKSPACE_SERVER_OOM_SCORE_ADJ)
            .is_err()
        {
            return;
        }
        write_oom_score_adj_raw(b"-500\n").unwrap();
        let after = std::fs::read_to_string("/proc/self/oom_score_adj").unwrap();
        assert_eq!(
            after.trim(),
            crate::daemonize::PREVIEW_PROXY_OOM_SCORE_ADJ.to_string(),
            "raise from -900 to -500 must land at preview score"
        );
        let _ = std::fs::write("/proc/self/oom_score_adj", format!("{restore}\n"));
    }

    #[test]
    fn record_restart_increments_the_labeled_counter() {
        // The reason → label wiring must be exact (no cross-wiring).
        assert_eq!(RestartReason::Exit.as_str(), "exit");
        assert_eq!(RestartReason::SpawnError.as_str(), "spawn_error");

        // Concurrency-safe: assert a strict increase (other tests may also bump
        // these labels), matching the crate's metric-test convention.
        let exit_before = PREVIEW_PROXY_RESTART_TOTAL
            .with_label_values(&["exit"])
            .get();
        let spawn_before = PREVIEW_PROXY_RESTART_TOTAL
            .with_label_values(&["spawn_error"])
            .get();

        record_restart(RestartReason::Exit);
        record_restart(RestartReason::SpawnError);

        assert!(
            PREVIEW_PROXY_RESTART_TOTAL
                .with_label_values(&["exit"])
                .get()
                > exit_before
        );
        assert!(
            PREVIEW_PROXY_RESTART_TOTAL
                .with_label_values(&["spawn_error"])
                .get()
                > spawn_before
        );
    }

    #[test]
    fn default_preview_control_port_is_6015() {
        assert_eq!(
            DEFAULT_PREVIEW_CONTROL_PORT, 6015,
            "guards only this local fallback; keep in step with the proxy's --control-port default"
        );
    }

    #[test]
    fn metrics_url_targets_the_loopback_control_metrics_path() {
        assert_eq!(
            metrics_url(6015),
            "http://127.0.0.1:6015/__control/metrics",
            "must match the proxy's control router metrics route"
        );
    }

    #[tokio::test]
    async fn metrics_scrape_loop_donates_the_body_immediately_and_stops_on_shutdown() {
        const BODY: &str = "preview_proxy_active_ws_connections 3\n";
        let port = serve_canned("HTTP/1.1 200 OK", BODY, true).await;
        let donated = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (tx, rx) = watch::channel(false);
        let sink = Arc::clone(&donated);
        let handle = tokio::spawn(scrape_metrics_loop(
            port,
            // 1h interval: only the immediate first scrape can donate.
            Duration::from_secs(3600),
            rx,
            move |body| sink.lock().unwrap().push(body.to_owned()),
        ));

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !donated.lock().unwrap().is_empty() {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the first scrape must donate without waiting a full interval");
        assert_eq!(vec![BODY.to_owned()], *donated.lock().unwrap());

        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("metrics scrape loop must stop promptly on shutdown")
            .expect("task should not panic");
    }

    #[tokio::test]
    async fn metrics_scrape_loop_skips_error_responses_and_absent_proxy() {
        let port = serve_canned("HTTP/1.1 500 Internal Server Error", "boom", true).await;
        let donated = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = watch::channel(false);
        let counter = Arc::clone(&donated);
        let handle = tokio::spawn(scrape_metrics_loop(
            port,
            Duration::from_millis(5),
            rx,
            move |_| {
                counter.fetch_add(1, SeqCst);
            },
        ));
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(0, donated.load(SeqCst), "error responses must not donate");
        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("must stop on shutdown")
            .expect("no panic");

        let donated = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = watch::channel(false);
        let counter = Arc::clone(&donated);
        let closed = ReservedRefusedPort::open();
        let handle = tokio::spawn(scrape_metrics_loop(
            closed.port,
            Duration::from_millis(5),
            rx,
            move |_| {
                counter.fetch_add(1, SeqCst);
            },
        ));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(0, donated.load(SeqCst), "an absent proxy must not donate");
        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("must stop on shutdown")
            .expect("no panic");
    }

    #[tokio::test]
    async fn metrics_scrape_loop_returns_immediately_when_already_shut_down() {
        let (_tx, rx) = watch::channel(true);
        tokio::time::timeout(
            Duration::from_secs(5),
            scrape_metrics_loop(6015, Duration::from_millis(5), rx, |_| {
                panic!("must not scrape after a pre-flipped shutdown")
            }),
        )
        .await
        .expect("a pre-flipped shutdown must return without scraping");
    }

    /// What an older proxy binary reports.
    fn stamp_only(last_activity_ms: u64) -> ActivitySample {
        ActivitySample {
            last_activity_ms,
            ..Default::default()
        }
    }

    #[test]
    fn parse_activity_body_reads_stamp_and_rejects_bad_shapes() {
        assert_eq!(
            parse_activity_body(r#"{"last_activity_ms":1234}"#),
            Some(stamp_only(1234))
        );
        assert_eq!(
            parse_activity_body(r#"{"last_activity_ms":0,"extra":true}"#),
            Some(stamp_only(0))
        );
        assert_eq!(parse_activity_body(r#"{"other":1}"#), None);
        assert_eq!(parse_activity_body(r#"{"last_activity_ms":"7"}"#), None);
        assert_eq!(parse_activity_body(r#"{"last_activity_ms":1.5}"#), None);
        assert_eq!(parse_activity_body("not json"), None);
        assert_eq!(parse_activity_body(""), None);
    }

    /// The binaries are version-pinned per session, but a restore can repin, so
    /// this skew is real rather than theoretical.
    #[test]
    fn parse_activity_body_tolerates_a_proxy_without_the_new_fields() {
        let old = parse_activity_body(r#"{"last_activity_ms":5,"status_holds_in_use":2}"#)
            .expect("an old proxy body must still parse");
        assert_eq!(old.last_routed_ms, 0);
        assert_eq!(old.ws_tunnels_open, 0);
        assert_eq!(
            old.routed_requests_in_flight, 0,
            "absent fields read as 'nothing attached', never as attached"
        );
    }

    /// A WS-only client has no stamps, so the attached counters are its only
    /// hold. One unreachable scrape — a proxy restart, a loopback hiccup — must
    /// not drop it and publish idle; a sustained absence still must.
    #[tokio::test]
    async fn one_absent_scrape_does_not_drop_an_attached_client() {
        let tracker = TestSink::with_window_ms(10_000);
        tracker.set_preview_attached(1, 0);

        // Nothing is listening on this port, so every scrape classifies Absent.
        let closed = ReservedRefusedPort::open();
        let (tx, rx) = watch::channel(false);
        let loop_handle = tokio::spawn(scrape_activity_loop(
            closed.port,
            sink_of(&tracker),
            Duration::from_millis(10),
            rx,
        ));

        // Many consecutive absences, all well inside the 10s grace.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            tracker.attached(),
            (1, 0),
            "repeated unreachable scrapes inside the grace must not drop the hold"
        );

        let _ = tx.send(true);
        let _ = loop_handle.await;
    }

    /// A proxy that answers unusably tells us nothing about attached clients
    /// either, and the mirrored counters do not decay on their own — so an
    /// endless run of error statuses must not pin `PreviewAttached` forever.
    #[tokio::test]
    async fn a_persistently_broken_proxy_does_not_pin_attached_forever() {
        let tracker = TestSink::with_window_ms(50);
        tracker.set_preview_attached(1, 0);

        // Answers every time, always with a 500 => BadResponse, never Absent.
        let port = serve_canned("HTTP/1.1 500 Internal Server Error", "boom", true).await;
        let (tx, rx) = watch::channel(false);
        let loop_handle = tokio::spawn(scrape_activity_loop(
            port,
            sink_of(&tracker),
            Duration::from_millis(10),
            rx,
        ));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tracker.ws_tunnels_open() != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "a sustained run of unusable responses must age the hold out"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let _ = tx.send(true);
        let _ = loop_handle.await;
    }

    /// The other half: a proxy that stays gone must eventually release, or a
    /// tunnel that died with it would hold the sandbox to the TTL.
    #[tokio::test]
    async fn a_sustained_absence_clears_the_attached_client() {
        let tracker = TestSink::with_window_ms(50);
        tracker.set_preview_attached(1, 0);

        let closed = ReservedRefusedPort::open();
        let (tx, rx) = watch::channel(false);
        let loop_handle = tokio::spawn(scrape_activity_loop(
            closed.port,
            sink_of(&tracker),
            Duration::from_millis(10),
            rx,
        ));

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tracker.ws_tunnels_open() != 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "an absence past the window must clear the attached counters"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        let _ = tx.send(true);
        let _ = loop_handle.await;
    }

    #[test]
    fn parse_activity_body_reads_the_attached_client_fields() {
        let sample = parse_activity_body(
            r#"{"last_activity_ms":9,"status_holds_in_use":1,
                "held_status_aborts_quieted":0,"ws_tunnels_open":3,
                "routed_requests_in_flight":2,"last_routed_ms":7}"#,
        )
        .expect("parse");
        assert_eq!(sample.last_activity_ms, 9);
        assert_eq!(sample.last_routed_ms, 7);
        assert_eq!(sample.ws_tunnels_open, 3);
        assert_eq!(sample.routed_requests_in_flight, 2);
    }

    #[test]
    fn classify_activity_response_distinguishes_stamp_from_bad() {
        assert_eq!(
            classify_activity_response(200, r#"{"last_activity_ms":42}"#),
            ScrapeOutcome::Stamp(stamp_only(42))
        );
        assert_eq!(
            classify_activity_response(200, "garbage"),
            ScrapeOutcome::BadResponse
        );
        assert_eq!(
            classify_activity_response(204, ""),
            ScrapeOutcome::BadResponse
        );
        for status in [301u16, 302, 400, 404, 500, 503] {
            assert_eq!(
                classify_activity_response(status, r#"{"last_activity_ms":42}"#),
                ScrapeOutcome::BadResponse,
                "a non-2xx ({status}) must not be read as a stamp"
            );
        }
    }

    #[test]
    fn preview_activity_advanced_detects_only_strictly_newer() {
        assert!(!preview_activity_advanced(0, 0));
        assert!(preview_activity_advanced(0, 1));
        assert!(preview_activity_advanced(5, 6));
        assert!(!preview_activity_advanced(5, 5));
        assert!(!preview_activity_advanced(5, 4));
    }

    fn scrape_client() -> reqwest::Client {
        #[allow(clippy::disallowed_methods)] // localhost preview server; TLS policy N/A
        reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build client")
    }

    /// Bind an ephemeral loopback port without `listen`. Connects are refused
    /// and no sibling test can steal the port — dropping a listener and racing
    /// `scrape_activity` previously let `serve_incrementing_stamp` take it and
    /// yield `Stamp { last_activity_ms: 1 }` instead of `Absent`.
    struct ReservedRefusedPort {
        port: u16,
        _socket: TcpSocket,
    }

    impl ReservedRefusedPort {
        fn open() -> Self {
            let socket = TcpSocket::new_v4().expect("open v4 tcp socket");
            socket
                .bind((Ipv4Addr::LOCALHOST, 0).into())
                .expect("bind ephemeral loopback");
            let port = socket.local_addr().expect("local addr").port();
            Self {
                port,
                _socket: socket,
            }
        }
    }

    async fn serve_canned(status_line: &'static str, body: &'static str, repeat: bool) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind ephemeral loopback");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
                if !repeat {
                    return;
                }
            }
        });
        port
    }

    async fn serve_incrementing_stamp() -> u16 {
        use std::sync::atomic::{AtomicU64, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind ephemeral loopback");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            let counter = AtomicU64::new(1);
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let stamp = counter.fetch_add(1, Ordering::Relaxed);
                let body = format!(r#"{{"last_activity_ms":{stamp}}}"#);
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        port
    }

    async fn serve_accept_then_hang() -> u16 {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind ephemeral loopback");
        let port = listener.local_addr().expect("addr").port();
        tokio::spawn(async move {
            if let Ok((_sock, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
        port
    }

    #[tokio::test]
    async fn scrape_activity_returns_stamp_from_a_live_endpoint() {
        let port = serve_canned("HTTP/1.1 200 OK", r#"{"last_activity_ms":9876}"#, false).await;
        assert_eq!(
            scrape_activity(&scrape_client(), &activity_url(port)).await,
            ScrapeOutcome::Stamp(stamp_only(9876))
        );
    }

    #[tokio::test]
    async fn scrape_activity_classifies_error_status_as_bad_response() {
        let port = serve_canned("HTTP/1.1 500 Internal Server Error", "boom", false).await;
        assert_eq!(
            scrape_activity(&scrape_client(), &activity_url(port)).await,
            ScrapeOutcome::BadResponse
        );
    }

    #[tokio::test]
    async fn scrape_activity_treats_a_closed_port_as_absent_not_error() {
        let closed = ReservedRefusedPort::open();
        assert_eq!(
            scrape_activity(&scrape_client(), &activity_url(closed.port)).await,
            ScrapeOutcome::Absent,
            "a refused connection (proxy absent) must be a quiet no-op, not BadResponse"
        );
    }

    #[tokio::test]
    async fn scrape_activity_treats_a_hung_endpoint_as_absent() {
        let port = serve_accept_then_hang().await;
        #[allow(clippy::disallowed_methods)] // localhost preview server; TLS policy N/A
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(150))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("build client");
        assert_eq!(
            scrape_activity(&client, &activity_url(port)).await,
            ScrapeOutcome::Absent,
            "a connection that never responds must time out to Absent, not BadResponse"
        );
    }

    #[tokio::test]
    async fn scrape_loop_withholds_idle_on_advance_then_stops_on_shutdown() {
        let port = serve_incrementing_stamp().await;
        let tracker = TestSink::new();
        assert_eq!(tracker.activity_notes(), 0);

        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(scrape_activity_loop(
            port,
            sink_of(&tracker),
            Duration::from_millis(5),
            rx,
        ));

        let deadline = Instant::now() + Duration::from_secs(5);
        while tracker.activity_notes() == 0 {
            assert!(
                Instant::now() < deadline,
                "the scrape loop must report an advance, which withholds idle"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("scrape loop must stop promptly on shutdown")
            .expect("task should not panic");
    }

    #[tokio::test]
    async fn scrape_loop_keeps_idle_reported_on_error_responses() {
        let port = serve_canned("HTTP/1.1 500 Internal Server Error", "boom", true).await;
        let tracker = TestSink::new();
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(scrape_activity_loop(
            port,
            sink_of(&tracker),
            Duration::from_millis(5),
            rx,
        ));

        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            tracker.activity_notes(),
            0,
            "an error response must never be read as activity, so idle stays reported"
        );

        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("scrape loop must stop promptly on shutdown")
            .expect("task should not panic");
    }

    #[tokio::test]
    async fn scrape_loop_baselines_a_stale_stamp_without_withholding() {
        let port = serve_canned("HTTP/1.1 200 OK", r#"{"last_activity_ms":777}"#, true).await;
        let tracker = TestSink::new();
        let (tx, rx) = watch::channel(false);
        let handle = tokio::spawn(scrape_activity_loop(
            port,
            sink_of(&tracker),
            Duration::from_millis(5),
            rx,
        ));

        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            tracker.activity_notes(),
            0,
            "a constant (stale) stamp must only baseline, never count as an advance"
        );

        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("scrape loop must stop promptly on shutdown")
            .expect("task should not panic");
    }

    #[tokio::test]
    async fn scrape_loop_returns_immediately_when_already_shut_down() {
        let tracker = sink_of(&TestSink::new());
        let (_tx, rx) = watch::channel(true);
        tokio::time::timeout(
            Duration::from_secs(5),
            scrape_activity_loop(6015, tracker, Duration::from_millis(5), rx),
        )
        .await
        .expect("a pre-flipped shutdown must return without scraping");
    }

    /// A `tokio::process::Command` that exits immediately with `code`.
    fn fake_exit_command(code: i32) -> tokio::process::Command {
        use std::process::Stdio;
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(format!("exit {code}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    }

    /// A long-lived child that records its own (post-`exec`) PID to `pid_path`
    /// then blocks, so the test can assert the process is actually killed.
    #[cfg(unix)]
    fn fake_pid_recording_command(pid_path: &Path) -> tokio::process::Command {
        use std::process::Stdio;
        let mut cmd = tokio::process::Command::new("/bin/sh");
        // `$0` is the script's first positional arg; `exec sleep` keeps the PID.
        cmd.arg("-c")
            .arg(r#"echo $$ > "$0"; exec sleep 3600"#)
            .arg(pid_path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    }

    /// Poll `pid_path` until the child has written a valid PID.
    #[cfg(unix)]
    async fn read_recorded_pid(pid_path: &Path) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(text) = std::fs::read_to_string(pid_path)
                && let Ok(pid) = text.trim().parse::<i32>()
                && pid > 0
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "child never recorded its pid");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// `kill(pid, 0)` existence probe (sends no signal).
    #[cfg(unix)]
    fn process_alive(pid: i32) -> bool {
        // SAFETY: signal 0 only checks for the process's existence/permission.
        unsafe { libc::kill(pid, 0) == 0 }
    }

    /// Fast policy so backoff sleeps don't slow the real-time integration tests.
    fn fast_policy() -> BackoffPolicy {
        BackoffPolicy::new(Duration::from_millis(1), Duration::from_millis(2))
    }

    async fn wait_until(counter: &Arc<AtomicUsize>, at_least: usize) {
        loop {
            if counter.load(SeqCst) >= at_least {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }

    #[tokio::test]
    async fn supervisor_restarts_on_exit_then_stops_on_shutdown() {
        let (tx, rx) = watch::channel(false);
        let spawns = Arc::new(AtomicUsize::new(0));
        let factory = {
            let spawns = spawns.clone();
            move || {
                spawns.fetch_add(1, SeqCst);
                Ok(fake_exit_command(1))
            }
        };
        // healthy_run far above the child's lifetime ⇒ every exit is unhealthy.
        let handle = tokio::spawn(supervise_loop(
            factory,
            fast_policy(),
            Duration::from_secs(3600),
            rx,
        ));

        // The child is re-spawned after it exits (≥2 spawns ⇒ at least one
        // restart). Kept low to minimize fork churn in the parallel test runner;
        // the backoff progression itself is covered by the pure `next_step` test.
        tokio::time::timeout(Duration::from_secs(5), wait_until(&spawns, 2))
            .await
            .expect("supervisor should restart the child after it exits");

        // Cooperative shutdown: flipping the watch returns the task.
        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("supervisor should stop after shutdown")
            .expect("task should not panic");

        assert!(spawns.load(SeqCst) >= 2);
    }

    #[tokio::test]
    async fn supervisor_returns_cleanly_when_shutdown_races_a_child_exit() {
        // Best-effort guard for the post-`wait()` shutdown re-check: with a
        // fast-exiting child and shutdown flipped right after a spawn, the exit
        // and the shutdown land close together. `select!` is random, so we can't
        // deterministically force the re-check branch over the `changed()` arm,
        // but both must yield a prompt, panic-free return with no hang or leak.
        // Repeated to cover the timing window.
        for _ in 0..8 {
            let (tx, rx) = watch::channel(false);
            let spawns = Arc::new(AtomicUsize::new(0));
            let factory = {
                let spawns = spawns.clone();
                move || {
                    spawns.fetch_add(1, SeqCst);
                    Ok(fake_exit_command(0))
                }
            };
            let handle = tokio::spawn(supervise_loop(
                factory,
                fast_policy(),
                Duration::from_secs(3600),
                rx,
            ));
            wait_until(&spawns, 1).await;
            tx.send(true).expect("receiver alive");
            tokio::time::timeout(Duration::from_secs(5), handle)
                .await
                .expect("supervisor must return on a shutdown/exit race")
                .expect("task should not panic");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn supervisor_shutdown_kills_running_child_without_restart() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("child.pid");
        let (tx, rx) = watch::channel(false);
        let spawns = Arc::new(AtomicUsize::new(0));
        let factory = {
            let spawns = spawns.clone();
            let pid_path = pid_path.clone();
            move || {
                spawns.fetch_add(1, SeqCst);
                Ok(fake_pid_recording_command(&pid_path))
            }
        };
        let handle = tokio::spawn(supervise_loop(
            factory,
            fast_policy(),
            Duration::from_secs(1),
            rx,
        ));

        // The child is up and alive before we ask for shutdown.
        let pid = read_recorded_pid(&pid_path).await;
        assert!(
            process_alive(pid),
            "child should be running before shutdown"
        );

        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("supervisor should stop after shutdown")
            .expect("task should not panic");

        assert_eq!(
            spawns.load(SeqCst),
            1,
            "a live child is killed on shutdown, never restarted"
        );

        // Positively assert the child process is gone (SIGKILL + reap on the
        // shutdown path), not merely that the supervise task returned.
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(pid) {
            assert!(Instant::now() < deadline, "child {pid} survived shutdown");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn supervisor_survives_persistent_spawn_failure() {
        let (tx, rx) = watch::channel(false);
        let attempts = Arc::new(AtomicUsize::new(0));
        let factory = {
            let attempts = attempts.clone();
            move || -> io::Result<tokio::process::Command> {
                attempts.fetch_add(1, SeqCst);
                Err(io::Error::new(io::ErrorKind::NotFound, "no such binary"))
            }
        };
        let handle = tokio::spawn(supervise_loop(
            factory,
            fast_policy(),
            Duration::from_secs(3600),
            rx,
        ));

        // Repeated spawn failures must back off and retry, never drop the task.
        tokio::time::timeout(Duration::from_secs(5), wait_until(&attempts, 5))
            .await
            .expect("spawn failures should be retried");
        assert!(
            !handle.is_finished(),
            "the supervise task must not terminate on spawn failure"
        );

        tx.send(true).expect("receiver alive");
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("supervisor should still honor shutdown")
            .expect("task should not panic");
    }

    /// Helper-process env switch and the success exit code for the PDEATHSIG
    /// test below. A distinct (non-zero) success code so a filter that matched
    /// no test (libtest would exit 0) can't masquerade as a pass.
    #[cfg(target_os = "linux")]
    const PDEATHSIG_HELPER_ENV: &str = "ECHO_AGENT_PDEATHSIG_HELPER";
    #[cfg(target_os = "linux")]
    const PDEATHSIG_HELPER_OK: i32 = 42;

    /// End-to-end PDEATHSIG guard: a grandchild armed exactly like the proxy
    /// child (`PR_SET_PDEATHSIG(SIGKILL)` + `getppid` race-close) must not
    /// outlive its parent. Validates the actual kernel mechanism, not just
    /// wiring. (The "parent lives ⇒ child lives" direction is covered by
    /// `supervisor_shutdown_kills_running_child_without_restart`, where a child
    /// runs until the still-alive parent flips shutdown.)
    ///
    /// The fork(P)/fork(G) scenario runs in a freshly **re-exec'd** helper
    /// process, not the test process: the test binary's descriptors are
    /// `O_CLOEXEC`, so the helper's exec closes them all (including other
    /// concurrent tests' `flock`'d pidfiles). Forking only that isolated process
    /// therefore can't pin a lock another test holds — the hazard of forking the
    /// multi-threaded test runner directly.
    #[cfg(target_os = "linux")]
    #[test]
    fn pdeathsig_does_not_let_a_child_outlive_its_parent() {
        // Helper mode: run the scenario in this isolated process and exit with
        // the verdict.
        if std::env::var_os(PDEATHSIG_HELPER_ENV).is_some() {
            std::process::exit(run_pdeathsig_scenario());
        }

        // Driver mode: launch the helper and assert its verdict. stdio is
        // silenced so the nested libtest banner doesn't pollute this run's output.
        let exe = std::env::current_exe().expect("current_exe");
        let status = std::process::Command::new(exe)
            .arg("pdeathsig_does_not_let_a_child_outlive_its_parent") // unique substring filter
            .arg("--nocapture")
            .env(PDEATHSIG_HELPER_ENV, "1")
            // The helper is a fresh libtest run of exactly the one filtered test.
            // Strip Bazel's per-shard test env so that when this target is built
            // with `shard_count > 1`, the re-exec'd helper does not re-apply
            // sharding to its single filtered test — otherwise the test could be
            // partitioned into a shard other than the inherited TEST_SHARD_INDEX,
            // run zero tests, and exit 0 instead of PDEATHSIG_HELPER_OK, failing
            // the driver's verdict assertion. Also drop the inherited test filter
            // so only our positional filter selects the test.
            .env_remove("TEST_SHARD_INDEX")
            .env_remove("TEST_TOTAL_SHARDS")
            .env_remove("TEST_SHARD_STATUS_FILE")
            .env_remove("TESTBRIDGE_TEST_ONLY")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn pdeathsig helper process");
        assert_eq!(
            status.code(),
            Some(PDEATHSIG_HELPER_OK),
            "armed grandchild must not outlive its parent (helper verdict {status:?})",
        );
    }

    /// fork P; P forks the armed grandchild G then dies; assert G terminates.
    /// Returns [`PDEATHSIG_HELPER_OK`] on success, `1` otherwise. Runs only in
    /// the isolated helper process (no concurrent tests, no inherited locks).
    #[cfg(target_os = "linux")]
    fn run_pdeathsig_scenario() -> i32 {
        const FAIL: i32 = 1;
        // SAFETY: after each fork the child/grandchild call only async-signal-safe
        // libc functions (`prctl`, `getppid`, `pause`, `close`, `write`,
        // `_exit`); no allocation and no locks.
        unsafe {
            // Subreaper so G reparents to us when P dies, letting us reap it.
            libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1 as libc::c_ulong);

            let mut fds = [0i32; 2];
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return FAIL;
            }
            let (rfd, wfd) = (fds[0], fds[1]);

            let intermediate = libc::fork();
            if intermediate < 0 {
                return FAIL;
            }
            if intermediate == 0 {
                // Intermediate parent P — stand-in for the workspace-server.
                libc::close(rfd);
                let p_pid = libc::getpid();
                let grandchild = libc::fork();
                if grandchild == 0 {
                    // Grandchild G — stand-in for the proxy child.
                    libc::close(wfd);
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
                    if libc::getppid() != p_pid {
                        libc::_exit(0); // race-close: P already gone.
                    }
                    loop {
                        libc::pause(); // wait for the PDEATHSIG SIGKILL.
                    }
                }
                // P: report G's pid, then die so PDEATHSIG fires on G.
                let _ = libc::write(
                    wfd,
                    &grandchild as *const i32 as *const libc::c_void,
                    std::mem::size_of::<i32>(),
                );
                libc::close(wfd);
                libc::_exit(0);
            }

            libc::close(wfd);
            let mut grandchild: i32 = -1;
            let n = libc::read(
                rfd,
                &mut grandchild as *mut i32 as *mut libc::c_void,
                std::mem::size_of::<i32>(),
            );
            libc::close(rfd);
            if n as usize != std::mem::size_of::<i32>() || grandchild <= 0 {
                return FAIL;
            }

            let mut status = 0i32;
            libc::waitpid(intermediate, &mut status, 0); // reap P

            // Once P dies G must terminate (SIGKILL via PDEATHSIG, or exit(0)
            // via the race-close). We reap it as the subreaper, or — if it
            // reparented to init — `kill(_, 0)` reports `ESRCH`.
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if libc::waitpid(grandchild, &mut status, libc::WNOHANG) == grandchild {
                    return PDEATHSIG_HELPER_OK;
                }
                if libc::kill(grandchild, 0) == -1 {
                    return PDEATHSIG_HELPER_OK;
                }
                if Instant::now() >= deadline {
                    libc::kill(grandchild, libc::SIGKILL);
                    let _ = libc::waitpid(grandchild, &mut status, 0);
                    return FAIL;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}
