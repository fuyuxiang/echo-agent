//! HTTP clients for the application.
//!
//! Building a `reqwest::Client` is expensive (~95ms) because it loads
//! TLS root certificates from the OS trust store. This module
//! provides four clients for non-sampling traffic (the first three
//! public and cached, the last crate-internal and built on demand):
//!
//! - `shared_client`: a `OnceLock`-cached async client for general
//!   use (telemetry, feedback, settings, etc.).
//! - `shared_upload_client`: a `OnceLock`-cached client for GCS
//!   uploads with aggressive connection pool eviction.
//! - `shared_startup_blocking_client`: a blocking client for the early
//!   model prefetch (runs before the async runtime is available).
//! - `fresh_http1_client` -- a crate-internal, on-demand, pool-less
//!   HTTP/1.1 client used by `send_with_retry_escaping_pool` for the
//!   final retry attempt to escape a poisoned pool within a tight budget.
//!
//! Sampling traffic uses process-wide shared clients owned by
//! `echo_agent_sampler::shared_http` (one HTTP/2 pooled client plus
//! a pool-less HTTP/1.1 fallback shared across every
//! `SamplingClient`). The sampler reads `ECHO_AGENT_POOL_*` /
//! `ECHO_AGENT_CONNECT_TIMEOUT_SECS` once, when its shared client is
//! first built, and `ECHO_AGENT_SAMPLER_SHARED_CLIENT=0` falls back to
//! a fresh client per `SamplingClient`.
//!
//! TLS policy (backend pin, roots, provider) lives in `echo_agent_extra_ca`.

use std::sync::OnceLock;

use echo_agent_workspace::permission::ClientType;

/// Per-attempt ceiling for a startup `/settings` or `/v1/models` fetch; raising
/// it delays how soon the background refresh gives up and retries.
pub const STARTUP_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Cap on non-interactive boot auth (token refresh or cold-start mint); a mint
/// that exceeds it leaves the leader session-less and is retried off the
/// readiness path.
pub const STARTUP_AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Ceiling on a single startup token-refresh round trip, kept separate from
/// `STARTUP_FETCH_TIMEOUT` so the two tune independently; on timeout the caller
/// proceeds with cached or no credentials and re-auths later.
pub const STARTUP_AUTH_REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// Outer bound on a single settings-reapply task, which drives up to
/// `SETTINGS_FETCH_MAX_ATTEMPTS` fetches.
pub const SETTINGS_REAPPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Attempt budget for the background settings fetch; bounds proxy load while
/// still covering a brief blip.
pub const SETTINGS_FETCH_MAX_ATTEMPTS: u32 = 3;
// A `401` self-heal may add one more bounded fetch beyond this cap; that fetch
// is cut off fail-closed and retried later, so the cap only needs to cover the
// common path.
const _: () = assert!(
    SETTINGS_REAPPLY_TIMEOUT.as_millis()
        > STARTUP_FETCH_TIMEOUT.as_millis() * (1 + SETTINGS_FETCH_MAX_ATTEMPTS as u128),
    "SETTINGS_REAPPLY_TIMEOUT must exceed STARTUP_FETCH_TIMEOUT * (1 + MAX_ATTEMPTS)"
);

/// Lower bound for a client's leader-connect timeout: a slow-but-valid boot
/// (bounded startup auth plus the rest of leader startup and the connect
/// handshake) must never be aborted. The pager bounds its connect by this value,
/// reached via the shell's `http` re-export.
pub const MIN_CLIENT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
const _: () = assert!(
    MIN_CLIENT_CONNECT_TIMEOUT.as_millis() >= 2 * STARTUP_AUTH_TIMEOUT.as_millis(),
    "MIN_CLIENT_CONNECT_TIMEOUT must stay >= 2x STARTUP_AUTH_TIMEOUT"
);

/// Startup span timer, local to this crate.
///
/// Replaces `echo_agent_runtime::instrumentation_timer!`, which cannot be referenced
/// here (it lives in the shell crate, which now depends on this one). This is a
/// behavior-preserving copy: it routes to the same
/// `echo_agent_telemetry::instrumentation` API and keeps the Chrome trace
/// span for these startup timings.
macro_rules! startup_timer {
    ($name:literal) => {{
        use echo_agent_telemetry::instrumentation::{
            InstrumentationMode, InstrumentationTimer, TARGET, current_mode,
        };
        let mode = current_mode();
        match mode {
            InstrumentationMode::Chrome => {
                let span = tracing::info_span!(target: TARGET, $name);
                InstrumentationTimer::new_with_span($name, mode, Some(span.entered()))
            }
            _ => InstrumentationTimer::new($name),
        }
    }};
}

static CLIENT_TYPE: OnceLock<ClientType> = OnceLock::new();

// `OriginClientInfo` is owned by `echo-agent-sampler` so `SamplerConfig` can use
// it without taking a circular dependency on `echo-agent-runtime`. Re-exported
// under the same path (`crate::http::OriginClientInfo`) so existing call-sites
// compile unchanged. The telemetry engine in `echo-agent-telemetry` consumes
// the same type via `echo_agent_sampler::OriginClientInfo`. The shell-specific
// constructors that depended on `ClientType` (a shell-only type) are free
// functions below.
pub use echo_agent_sampler::OriginClientInfo;

/// Construct an [`OriginClientInfo`] from `ECHO_AGENT_CLIENT_NAME` /
/// `ECHO_AGENT_CLIENT_VERSION` env vars. Returns `None` when
/// `ECHO_AGENT_CLIENT_NAME` is unset.
pub fn origin_client_info_from_env() -> Option<OriginClientInfo> {
    std::env::var("ECHO_AGENT_CLIENT_NAME")
        .ok()
        .map(|product| OriginClientInfo {
            product,
            version: std::env::var("ECHO_AGENT_CLIENT_VERSION").ok(),
        })
}

/// Construct an [`OriginClientInfo`] from a shell-side
/// [`ClientType`] (which carries its UA label) and an optional
/// version string.
pub fn origin_client_info_from_client_type(
    client_type: ClientType,
    version: Option<String>,
) -> OriginClientInfo {
    OriginClientInfo {
        product: client_type.user_agent_label().to_string(),
        version,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlatformInfo {
    os: String,
    arch: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct UserAgent {
    origin: OriginClientInfo,
    agent_product: &'static str,
    agent_version: String,
    platform: PlatformInfo,
}

impl PlatformInfo {
    fn current() -> Self {
        let os = match std::env::consts::OS {
            "macos" => "macos",
            "windows" => "windows",
            other => other,
        }
        .to_string();

        let arch = match std::env::consts::ARCH {
            "arm64" => "aarch64",
            "x86_64" => "x86_64",
            other => other,
        }
        .to_string();

        Self { os, arch }
    }
}

impl UserAgent {
    fn render(&self) -> String {
        if self.origin.product == self.agent_product
            && self.origin.version.as_deref() == Some(self.agent_version.as_str())
        {
            return format!(
                "{}/{} ({}; {})",
                self.agent_product, self.agent_version, self.platform.os, self.platform.arch,
            );
        }

        match self.origin.version.as_deref() {
            Some(origin_version) => format!(
                "{}/{} {}/{} ({}; {})",
                self.origin.product,
                origin_version,
                self.agent_product,
                self.agent_version,
                self.platform.os,
                self.platform.arch,
            ),
            None => format!(
                "{} {}/{} ({}; {})",
                self.origin.product,
                self.agent_product,
                self.agent_version,
                self.platform.os,
                self.platform.arch,
            ),
        }
    }
}

fn agent_version() -> String {
    echo_agent_version::VERSION.to_string()
}

/// Set the process-level fallback origin client type for `User-Agent`.
pub fn set_client_name(client_type: ClientType) {
    CLIENT_TYPE
        .set(client_type)
        .expect("set_client_name called more than once");
}

pub fn process_user_agent_string() -> String {
    let agent_version = agent_version();
    let origin = origin_client_info_from_env().unwrap_or_else(|| {
        origin_client_info_from_client_type(
            CLIENT_TYPE.get().copied().unwrap_or(ClientType::Generic),
            Some(agent_version.clone()),
        )
    });

    UserAgent {
        origin,
        agent_product: "echo-agent-shell",
        agent_version,
        platform: PlatformInfo::current(),
    }
    .render()
}

pub fn session_user_agent_string(origin: &OriginClientInfo) -> String {
    UserAgent {
        origin: origin.clone(),
        agent_product: "echo-agent-shell",
        agent_version: agent_version(),
        platform: PlatformInfo::current(),
    }
    .render()
}

pub fn origin_client_info_from_meta(
    meta: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<OriginClientInfo> {
    let product = meta
        .and_then(|m| m.get("clientIdentifier"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            meta.and_then(|m| m.get("clientType"))
                .and_then(|v| serde_json::from_value::<ClientType>(v.clone()).ok())
                .map(|client_type| client_type.user_agent_label().to_string())
        });

    product.map(|product| OriginClientInfo {
        product,
        version: meta
            .and_then(|m| m.get("clientVersion"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
    })
}

pub fn merge_origin_client_info(
    primary: Option<OriginClientInfo>,
    fallback: Option<OriginClientInfo>,
) -> Option<OriginClientInfo> {
    match (primary, fallback) {
        (Some(primary), Some(fallback)) => Some(OriginClientInfo {
            product: primary.product,
            version: primary.version.or(fallback.version),
        }),
        (Some(primary), None) => Some(primary),
        (None, Some(fallback)) => Some(fallback),
        (None, None) => None,
    }
}

pub fn client_type_from_origin(origin: Option<&OriginClientInfo>) -> ClientType {
    ClientType::from_client_identifier(origin.map(|o| o.product.as_str()))
}

/// Process-level client identifier (`ECHO_AGENT_CLIENT_NAME` env var, default `"echo-agent-shell"`).
pub fn process_client_identifier() -> String {
    std::env::var("ECHO_AGENT_CLIENT_NAME").unwrap_or_else(|_| "echo-agent-shell".to_string())
}

/// Header telling cli-chat-proxy whether this process is a single-prompt
/// (`echoagent -p`) run or an interactive session; feeds the `client_mode`
/// metric label.
pub const CLIENT_MODE_HEADER: &str = "x-echo-agent-client-mode";

/// One-way latch: set to `"headless"` at startup by the non-TUI entry points
/// (`run_single_turn` for `echoagent -p`, `run_headless_inner` for
/// `echoagent agent [headless]`), `"interactive"` otherwise.
static CLIENT_MODE: OnceLock<&'static str> = OnceLock::new();

/// Mark this process as headless (single-prompt). No-op if already set.
pub fn set_process_client_mode_headless() {
    let _ = CLIENT_MODE.set("headless");
}

/// The mode sent in [`CLIENT_MODE_HEADER`]; defaults to `"interactive"`.
pub fn process_client_mode() -> &'static str {
    CLIENT_MODE.get().copied().unwrap_or("interactive")
}

pub fn user_agent_string_for(origin: &OriginClientInfo) -> String {
    session_user_agent_string(origin)
}

/// Returns a shared [`reqwest::Client`], creating it on first call.
///
/// The returned client is a cheap `Arc` clone — safe to pass across threads
/// and tasks. Sets a 30-second connect timeout; callers should set
/// per-request timeouts as needed.
///
/// Keeps HTTP/2 + connection pooling, but adds health-checks so a half-dead
/// pooled connection is detected and dropped instead of reused. Through an
/// LB/Cloudflare/proxy a kept-alive connection can be silently dropped upstream;
/// without these, reqwest reuses it and mints doomed streams on it, so every
/// retry fails identically and a reachable server looks unreachable. Idle/TCP
/// eviction drops connections before the upstream idle window (~60-100s; 30s is
/// a conservative default) closes them, and the HTTP/2 keepalive ping detects a
/// dead connection so the pool stops handing it out.
pub fn shared_client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            let _timer = startup_timer!("startup.http_client_build");
            echo_agent_extra_ca::build_reqwest_client(|builder| {
                builder
                    .connect_timeout(std::time::Duration::from_secs(30))
                    .user_agent(process_user_agent_string())
                    .pool_idle_timeout(std::time::Duration::from_secs(30))
                    .http2_keep_alive_interval(std::time::Duration::from_secs(20))
                    .http2_keep_alive_timeout(std::time::Duration::from_secs(10))
                    .http2_keep_alive_while_idle(true)
                    .tcp_keepalive(std::time::Duration::from_secs(30))
            })
            .expect("failed to build shared HTTP client")
        })
        .clone()
}

/// Wrap a raw client with [`AuthRetryMiddleware`] for automatic 401 retry.
pub fn with_auth_retry(
    client: reqwest::Client,
    credentials: std::sync::Arc<dyn echo_agent_auth::AuthCredentialProvider>,
) -> reqwest_middleware::ClientWithMiddleware {
    reqwest_middleware::ClientBuilder::new(client)
        .with(echo_agent_auth::AuthRetryMiddleware::new(credentials, 1))
        .build()
}

/// Returns a shared [`reqwest::Client`] for GCS uploads, creating it on first call.
///
/// Unlike `shared_client()`, this client has aggressive connection pool eviction
/// to avoid reusing stale/poisoned connections during retry loops. When uploads
/// fail and trigger exponential backoff (1s, 2s, 4s...), idle connections may be
/// closed by the server, Cloudflare, or load balancers. Without pool eviction,
/// all retries would reuse the same dead connection and fail.
///
/// Settings:
/// - HTTP/1.1 only — avoids HTTP/2 connection-poisoning where a degraded
///   multiplexed connection silently drops multipart request bodies, causing
///   cascading 400 errors across all concurrent uploads
/// - Small connection pool (2 per host) for parallel chunk uploads
/// - Short idle timeout (10s) to evict stale connections before backoff completes
pub fn shared_upload_client() -> reqwest::Client {
    static UPLOAD_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    UPLOAD_CLIENT
        .get_or_init(|| {
            echo_agent_extra_ca::build_reqwest_client(|builder| {
                builder
                    .http1_only()
                    .pool_max_idle_per_host(2)
                    .pool_idle_timeout(std::time::Duration::from_secs(10))
                    .user_agent(process_user_agent_string())
            })
            .expect("failed to build shared upload HTTP client")
        })
        .clone()
}

/// A fresh, pool-less HTTP/1.1 [`reqwest::Client`], deliberately NOT cached:
/// `pool_max_idle_per_host(0)` so each request opens a new connection, and no
/// connect timeout (callers bound each request with their own total timeout). The retry escape
/// policy that reaches for this client to dodge a poisoned pool lives on `send_with_retry_escaping_pool`.
///
/// Fallible: build can fail under fd/TLS pressure; the caller must not
/// panic on error (fallback policy lives at the call site).
pub(crate) fn fresh_http1_client() -> reqwest::Result<reqwest::Client> {
    echo_agent_extra_ca::build_reqwest_client(|builder| {
        builder
            .http1_only()
            .pool_max_idle_per_host(0)
            .user_agent(process_user_agent_string())
    })
}

/// Joins an error's `source()` chain into one string. A `reqwest::Error`'s `Display`
/// shows only the outer "error sending request for url (...)", hiding the real hyper
/// cause (reset, closed-before-complete, timeout) reachable only via `source()`.
pub fn error_cause_chain(err: &dyn std::error::Error) -> String {
    let mut msg = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        msg.push_str(": ");
        msg.push_str(&cause.to_string());
        source = cause.source();
    }
    msg
}

/// First OS error code in `err`'s `source()` chain (e.g. 104 `ECONNRESET` on
/// Linux, 10054 on Windows), preferring [`std::io::Error::raw_os_error`] and
/// falling back to the `(os error N)` suffix `io::Error`'s `Display` appends.
///
/// The fallback is load-bearing: a reset during the TLS handshake arrives as a
/// *custom* `io::Error` (kind `Other`, no raw code) whose only record of the
/// code is that suffix, and without it a rustls reset is indistinguishable
/// from an unreachable host.
///
/// `+ 'static` because `downcast_ref` resolves the type through
/// [`std::any::Any`], whose type ids only exist for `'static` types.
pub fn find_os_error_code(err: &(dyn std::error::Error + 'static)) -> Option<i32> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(code) = e.downcast_ref::<std::io::Error>().and_then(|ioe| {
            ioe.raw_os_error()
                .or_else(|| parse_os_error(&ioe.to_string()))
        }) {
            return Some(code);
        }
        cur = e.source();
    }
    None
}

/// Extract `N` from a message ending in `(os error N)`.
fn parse_os_error(msg: &str) -> Option<i32> {
    msg.rsplit_once("(os error ")?
        .1
        .trim_end_matches(')')
        .parse()
        .ok()
}

/// How a `reqwest` request/send failure should be treated by a retry loop.
#[derive(Debug, PartialEq, Eq)]
pub enum TransportFailureKind {
    /// The connection could never be established: the server is down or unreachable. Retryable.
    Unreachable,
    /// The server certificate's issuer is not trusted; installing the root CA fixes it. Not retryable.
    CertificateUntrusted,
    /// The server certificate is otherwise invalid (expired, wrong hostname); installing a root will not fix it. Not retryable.
    CertificateInvalid,
    /// An established request was cut short (timeout, reset, GOAWAY). Retryable.
    Interrupted,
    /// A client-side defect (builder error, redirect-policy violation). Not retryable.
    Permanent,
}

/// A classified `reqwest` request/send failure: a [`TransportFailureKind`] plus the
/// joined cause-chain detail. Derives `PartialEq` so the kind-to-error mapping can
/// be unit-tested by constructing values directly.
#[derive(Debug, PartialEq)]
pub struct TransportFailure {
    pub kind: TransportFailureKind,
    pub detail: String,
}

impl TransportFailure {
    /// Order matters: a certificate failure also satisfies `is_connect()`,
    /// and a connect failure is also `Kind::Request`.
    pub fn classify(e: &reqwest::Error) -> Self {
        let kind = transport_kind(
            certificate_error(e),
            e.is_connect(),
            e.is_timeout() || e.is_request() || e.is_body(),
        );
        Self {
            kind,
            detail: error_cause_chain(e),
        }
    }
}

/// The kind for a classified failure. Split from [`TransportFailure::classify`]
/// so the mapping is unit-testable without forging a `reqwest::Error`. Order
/// matters: a certificate failure also satisfies `is_connect()`, and a connect
/// failure also looks like a request error, so cert precedes connect precedes
/// interrupted.
fn transport_kind(
    cert: Option<CertVerdict>,
    is_connect: bool,
    is_interrupted: bool,
) -> TransportFailureKind {
    match cert {
        Some(CertVerdict::UntrustedIssuer) => TransportFailureKind::CertificateUntrusted,
        Some(CertVerdict::Other) => TransportFailureKind::CertificateInvalid,
        None if is_connect => TransportFailureKind::Unreachable,
        None if is_interrupted => TransportFailureKind::Interrupted,
        None => TransportFailureKind::Permanent,
    }
}

/// A rustls certificate-verification failure found in a `reqwest` error chain.
#[derive(Debug, PartialEq, Eq)]
enum CertVerdict {
    /// The issuer is not in the trust store; installing the root CA fixes it.
    UntrustedIssuer,
    /// Any other invalid certificate (expired, wrong name): non-retryable,
    /// but not fixable by installing a root.
    Other,
}

/// The certificate-verification failure in `err`'s cause chain, if any.
/// Descends into custom `io::Error` payloads, which `source()` skips.
fn certificate_error(err: &(dyn std::error::Error + 'static)) -> Option<CertVerdict> {
    let mut cur: Option<&(dyn std::error::Error + 'static)> = Some(err);
    while let Some(e) = cur {
        if let Some(rustls::Error::InvalidCertificate(cert)) = e.downcast_ref::<rustls::Error>() {
            return Some(match cert {
                rustls::CertificateError::UnknownIssuer => CertVerdict::UntrustedIssuer,
                _ => CertVerdict::Other,
            });
        }
        cur = match e
            .downcast_ref::<std::io::Error>()
            .and_then(|ioe| ioe.get_ref())
        {
            Some(payload) => Some(payload as &(dyn std::error::Error + 'static)),
            None => e.source(),
        };
    }
    None
}

/// Run `op` with bounded retries, swapping to a fresh pool-less client for the final attempt.
///
/// NOTE: this is not a plain retry loop — it bakes in a connection-escape policy. Early attempts run
/// on the pooled [`shared_client`] (HTTP/2 + keepalive + idle/TCP eviction); the FINAL attempt of a
/// multi-attempt run instead FORCES a fresh, pool-less HTTP/1.1 client (`fresh_http1_client`) so a
/// tight-budget caller (e.g. a 2-attempt login) can escape a half-dead pooled connection without
/// waiting out the pool's own keepalive/idle eviction (~20-30s).
///
/// This only rescues a FAST-FAIL connection (reset/GOAWAY/refused) within budget: the fresh attempt
/// returns quickly and succeeds. A silently black-holed connection still burns the caller's
/// per-request timeout on each attempt, so a tight deadline can elapse first and recovery defers to
/// the background sync loop / next start (best-effort, the documented behavior).
///
/// `op` receives the client to use and returns the WHOLE operation's result (send + body read +
/// decode), so a body-phase interruption is inside the retried unit, not just the send. `is_retryable`
/// decides whether a given error earns another attempt, so the caller keeps its own typed retry policy
/// (e.g. retry 5xx, fail fast on auth). `backoff(attempt)` is awaited before attempt N (N >= 1),
/// keeping this helper runtime-agnostic (the caller supplies the sleep). The client is passed by value
/// (a cheap `Arc` clone) so each attempt's future owns it instead of borrowing across the loop.
pub async fn send_with_retry_escaping_pool<T, E, Op, OpFut, Backoff, BackoffFut>(
    op: Op,
    max_attempts: u32,
    is_retryable: impl Fn(&E) -> bool,
    backoff: Backoff,
) -> Result<T, E>
where
    E: std::fmt::Display,
    Op: Fn(reqwest::Client) -> OpFut,
    OpFut: std::future::Future<Output = Result<T, E>>,
    Backoff: Fn(u32) -> BackoffFut,
    BackoffFut: std::future::Future<Output = ()>,
{
    // `max(1)` guarantees at least one attempt runs, so `last_err` is set if the loop falls through.
    let max_attempts = max_attempts.max(1);
    let pooled = shared_client();
    // Built lazily (loads OS TLS roots, ~95ms) and only if a final escape attempt is actually reached.
    let mut fresh: Option<reqwest::Client> = None;
    let mut last_err: Option<E> = None;

    for attempt in 0..max_attempts {
        if attempt > 0 {
            backoff(attempt).await;
        }
        // Only the final attempt of a multi-attempt run escapes onto a fresh pool-less connection; a
        // single-attempt caller keeps the pooled client (there is no prior failure to escape).
        let client = if attempt > 0 && attempt + 1 == max_attempts {
            match &fresh {
                Some(c) => c.clone(),
                None => match fresh_http1_client() {
                    Ok(c) => fresh.insert(c).clone(),
                    // Can't escape the pool (e.g. fd exhaustion); a pooled
                    // final attempt still beats aborting the process.
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to build pool-escape client; final attempt stays on pooled client");
                        pooled.clone()
                    }
                },
            }
        } else {
            pooled.clone()
        };
        match op(client).await {
            Ok(value) => return Ok(value),
            Err(e) if is_retryable(&e) => {
                // Log recovered-transient failures (a connection-health path); a silent retry would hide a degrading pool.
                tracing::debug!(attempt, error = %e, "send_with_retry_escaping_pool: retrying after transient failure");
                last_err = Some(e);
            }
            Err(e) => return Err(e),
        }
    }

    Err(last_err.expect("send_with_retry_escaping_pool ran at least one attempt"))
}

/// Shared blocking client for startup fetches. Carries `STARTUP_FETCH_TIMEOUT`
/// as the connect+read ceiling; do not reuse for long-lived requests.
///
/// This avoids redundant TLS certificate loading for blocking HTTP calls
/// (e.g., model prefetching during startup). The blocking client is separate
/// from the async `shared_client()` because reqwest's blocking client creates
/// its own internal tokio runtime.
///
/// Mirrors `shared_client()`'s pool self-healing for the same reason: this client
/// is reused (settings, prefetch) and a kept-alive connection an LB/Cloudflare/proxy
/// silently drops would otherwise be handed back out, so a reachable server looks
/// unreachable. Idle/TCP eviction drops a connection before the upstream idle window
/// (~60-100s; 30s is a conservative default) closes it. The HTTP/2 keepalive-ping
/// setters that `shared_client()` uses are NOT exposed on reqwest's blocking
/// `ClientBuilder` (0.12), so only the idle/TCP-eviction half applies here.
pub fn shared_startup_blocking_client() -> reqwest::blocking::Client {
    static BLOCKING_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
    BLOCKING_CLIENT
        .get_or_init(|| {
            let _timer = startup_timer!("startup.http_blocking_client_build");
            echo_agent_extra_ca::build_blocking_reqwest_client(|builder| {
                builder
                    .connect_timeout(STARTUP_FETCH_TIMEOUT)
                    .timeout(STARTUP_FETCH_TIMEOUT)
                    .user_agent(process_user_agent_string())
                    .pool_idle_timeout(std::time::Duration::from_secs(30))
                    .tcp_keepalive(std::time::Duration::from_secs(30))
            })
            .expect("failed to build shared blocking HTTP client")
        })
        .clone()
}

#[allow(clippy::disallowed_methods)] // test clients hit localhost mocks
#[cfg(test)]
mod tests {
    use super::*;

    /// The cause-chain formatter appends each `source()` joined with ": ", so a
    /// reqwest error whose `Display` hides the hyper cause still surfaces it.
    #[test]
    fn error_cause_chain_appends_hidden_sources() {
        #[derive(Debug)]
        struct Leaf;
        impl std::fmt::Display for Leaf {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "connection closed before message completed")
            }
        }
        impl std::error::Error for Leaf {}

        #[derive(Debug)]
        struct Wrapper(Leaf);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error sending request")
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        assert_eq!(
            error_cause_chain(&Wrapper(Leaf)),
            "error sending request: connection closed before message completed",
            "the hidden source cause must be appended after ': '"
        );
    }

    #[test]
    fn find_os_error_code_walks_source_chain() {
        #[derive(Debug)]
        struct IoLeaf(std::io::Error);
        impl std::fmt::Display for IoLeaf {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "io leaf")
            }
        }
        impl std::error::Error for IoLeaf {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        #[derive(Debug)]
        struct Wrapper(IoLeaf);
        impl std::fmt::Display for Wrapper {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "wrapper")
            }
        }
        impl std::error::Error for Wrapper {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }

        let err = Wrapper(IoLeaf(std::io::Error::from_raw_os_error(104)));
        assert_eq!(find_os_error_code(&err), Some(104));
        assert_eq!(find_os_error_code(&std::io::Error::other("no code")), None);
    }

    /// A TLS-handshake reset arrives as a custom `io::Error` with no raw code;
    /// live reqwest gives the chain `client error (Connect)` →
    /// `Connection reset by peer (os error 54)`.
    #[test]
    fn recovers_code_from_a_custom_io_error() {
        let tls_shaped = std::io::Error::other("Connection reset by peer (os error 54)");
        assert_eq!(tls_shaped.raw_os_error(), None, "precondition: no raw code");
        assert_eq!(find_os_error_code(&tls_shaped), Some(54));

        let windows_shaped = std::io::Error::other(
            "An existing connection was forcibly closed by the remote host. (os error 10054)",
        );
        assert_eq!(find_os_error_code(&windows_shaped), Some(10054));
    }

    /// Over a real socket: a mid-request reset must classify as `Interrupted`
    /// *and* surface the OS code, which is what lets a fleet report tell "peer
    /// reset us" from "server unreachable".
    ///
    /// Lives here rather than in a caller's crate: a `reqwest` client drags
    /// rustls into the test binary, which not every caller's tests tolerate.
    #[test]
    fn real_connection_reset_classifies_as_interrupted_with_os_code() {
        // Closing a socket whose receive queue still holds the request emits
        // RST instead of FIN.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let sock = listener.accept().expect("accept").0;
            sock.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .expect("read timeout");
            let _ = sock.peek(&mut [0u8; 64]);
            drop(sock);
        });

        let err = reqwest::blocking::Client::new()
            .get(format!("http://127.0.0.1:{port}/oauth2/device/code"))
            .send()
            .expect_err("reset must fail the request");

        assert_eq!(
            TransportFailure::classify(&err).kind,
            TransportFailureKind::Interrupted
        );
        assert!(
            // ECONNRESET: 54 on macOS, 104 on Linux, 10054 on Windows.
            matches!(find_os_error_code(&err), Some(54 | 104 | 10054)),
            "reset must carry an OS code, got {:?}",
            find_os_error_code(&err)
        );
    }

    #[test]
    fn certificate_errors_split_untrusted_issuer_from_other_invalid() {
        let wrap = |e: rustls::CertificateError| {
            std::io::Error::other(rustls::Error::InvalidCertificate(e))
        };
        assert_eq!(
            certificate_error(&wrap(rustls::CertificateError::UnknownIssuer)),
            Some(CertVerdict::UntrustedIssuer)
        );
        assert_eq!(
            certificate_error(&wrap(rustls::CertificateError::Expired)),
            Some(CertVerdict::Other)
        );
        assert_eq!(
            certificate_error(&wrap(rustls::CertificateError::NotValidForName)),
            Some(CertVerdict::Other)
        );
        assert_eq!(
            certificate_error(&std::io::Error::other("connection reset")),
            None
        );
    }

    #[test]
    fn transport_kind_maps_every_certificate_verdict_before_connect() {
        // An untrusted issuer gets the install-a-root path.
        assert_eq!(
            transport_kind(Some(CertVerdict::UntrustedIssuer), true, false),
            TransportFailureKind::CertificateUntrusted
        );
        // Expired/wrong-name is its own non-retryable kind, never Unreachable,
        // even though the underlying error also reports is_connect().
        assert_eq!(
            transport_kind(Some(CertVerdict::Other), true, false),
            TransportFailureKind::CertificateInvalid
        );
        assert_eq!(
            transport_kind(None, true, false),
            TransportFailureKind::Unreachable
        );
        assert_eq!(
            transport_kind(None, false, true),
            TransportFailureKind::Interrupted
        );
        assert_eq!(
            transport_kind(None, false, false),
            TransportFailureKind::Permanent
        );
    }

    #[test]
    fn untrusted_certificate_over_real_handshake_classifies_as_certificate_untrusted() {
        // A proxy would route the localhost request and misclassify.
        if ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"]
            .iter()
            .any(|v| std::env::var_os(v).is_some())
        {
            eprintln!("skipping: proxy environment set");
            return;
        }
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("cert");
        let server_config = rustls::ServerConfig::builder_with_provider(
            rustls::crypto::aws_lc_rs::default_provider().into(),
        )
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(cert.key_pair.serialize_der().into()),
        )
        .expect("server config");

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            let mut conn = rustls::ServerConnection::new(std::sync::Arc::new(server_config))
                .expect("server conn");
            let _ = conn.complete_io(&mut sock);
        });

        // The production client, so a lost `use_rustls_tls()` pin fails here.
        let err = shared_startup_blocking_client()
            .get(format!("https://localhost:{port}/"))
            .send()
            .expect_err("an untrusted certificate must fail the request");

        let failure = TransportFailure::classify(&err);
        assert_eq!(
            failure.kind,
            TransportFailureKind::CertificateUntrusted,
            "must not be mistaken for an unreachable server: {}",
            failure.detail
        );
    }

    #[test]
    fn parse_os_error_ignores_messages_without_a_code() {
        assert_eq!(
            parse_os_error("connection closed before message completed"),
            None
        );
        assert_eq!(
            parse_os_error("invalid peer certificate (os error oops)"),
            None
        );
        assert_eq!(parse_os_error("broken pipe (os error 32)"), Some(32));
    }

    #[test]
    fn origin_client_info_from_meta_extracts_identifier_and_version() {
        let meta = serde_json::json!({
            "clientIdentifier": "echo-agent-desktop",
            "clientVersion": "1.2.3",
        })
        .as_object()
        .cloned()
        .unwrap();
        assert_eq!(
            origin_client_info_from_meta(Some(&meta)),
            Some(OriginClientInfo {
                product: "echo-agent-desktop".to_string(),
                version: Some("1.2.3".to_string()),
            })
        );
    }

    #[test]
    fn origin_client_info_from_meta_uses_client_type_when_identifier_absent() {
        let meta = serde_json::json!({
            "clientType": "echo_agent_pager",
            "clientVersion": "0.1.2",
        })
        .as_object()
        .cloned()
        .unwrap();
        assert_eq!(
            origin_client_info_from_meta(Some(&meta)),
            Some(OriginClientInfo {
                product: "echo-agent-pager".to_string(),
                version: Some("0.1.2".to_string()),
            })
        );
    }

    #[test]
    fn merge_origin_client_info_preserves_primary_product_and_backfills_version() {
        let merged = merge_origin_client_info(
            Some(OriginClientInfo {
                product: "echo-agent-web".to_string(),
                version: None,
            }),
            Some(OriginClientInfo {
                product: "echo-agent-desktop".to_string(),
                version: Some("1.2.3".to_string()),
            }),
        );
        assert_eq!(
            merged,
            Some(OriginClientInfo {
                product: "echo-agent-web".to_string(),
                version: Some("1.2.3".to_string()),
            })
        );
    }

    #[test]
    fn session_user_agent_string_renders_expected_variants() {
        let with_version = session_user_agent_string(&OriginClientInfo {
            product: "echo-agent-desktop".to_string(),
            version: Some("1.2.3".to_string()),
        });
        assert!(with_version.starts_with("echo-agent-desktop/1.2.3 echo-agent-shell/"));
        assert!(with_version.contains(" ("));

        let without_version = session_user_agent_string(&OriginClientInfo {
            product: "echo-agent-web".to_string(),
            version: None,
        });
        assert!(without_version.starts_with("echo-agent-web echo-agent-shell/"));
        assert!(!without_version.starts_with("echo-agent-web/"));
    }

    #[test]
    fn user_agent_render_collapses_duplicate_origin_and_agent_identity() {
        let ua = UserAgent {
            origin: OriginClientInfo {
                product: "echo-agent-shell".to_string(),
                version: Some("0.1.171".to_string()),
            },
            agent_product: "echo-agent-shell",
            agent_version: "0.1.171".to_string(),
            platform: PlatformInfo {
                os: "macos".to_string(),
                arch: "aarch64".to_string(),
            },
        };

        assert_eq!(ua.render(), "echo-agent-shell/0.1.171 (macos; aarch64)");
    }
}
