//! REST client for uploading files to GCS via cli-chat-proxy.
//!
//! Routes requests through cli-chat-proxy using user's cloud.echo-agent.invalid auth token.
//! The proxy handles GCS authentication server-side.
//!
//! For large files that exceed Cloudflare's body size limit, use the multipart
//! upload API which splits files into chunks and composes them server-side.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::File as StdFile;
// Positional read traits live in different modules per platform; the
// methods we use (read_at on Unix, seek_read on Windows) have the same
// signature, so the call site cfg-branches on the method name only.
use echo_agent_auth::AuthCredentialProvider;
use echo_agent_circuit_breaker::{
    BreakerConfig, BreakerOpen, CircuitBreaker, Outcome, RetryPolicy,
};
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncRead;
use tokio::sync::Semaphore;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::bytes::Bytes;
use tokio_util::io::ReaderStream;

use crate::circuit_breaker_observer::TracingObserver;

// ============================================================================
// Storage Circuit Breaker policy
// ============================================================================
// `StorageClient`'s session-wide breaker uses the shared
// `echo_agent_circuit_breaker::BreakerConfig::client()` preset
// (sliding-window-with-min-samples: 5 samples / 60s window / 60s open
// duration / failure code = 401). The observer name "storage_breaker"
// is surfaced as a structured field on every tracing event so existing
// analytics queries continue to match.

const STORAGE_BREAKER_NAME: &str = "storage_breaker";

fn storage_breaker_config() -> BreakerConfig {
    BreakerConfig::client()
}

/// Hook invoked by [`StorageClient`] at every 401 response site so that
/// the embedding application can record auth-attribution telemetry.
///
/// Mirrors the pattern in `echo-agent-sampler::Auth401AttributionCallback`
/// and `echo-agent-tools::Auth401AttributionCallback`. The shell installs
/// a bridge implementation that wires into
/// `crate::auth::attribution::record_consumer_401`.
///
/// `sent_bearer_prefix` is the first-N characters of the bearer that was
/// actually sent on the wire (extracted at the trait boundary so the full
/// bearer never escapes `StorageClient`). `None` indicates no bearer was
/// configured (the unauthenticated test/CI path).
pub trait Auth401AttributionCallback: Send + Sync + std::fmt::Debug {
    fn record_401(&self, operation: &str, sent_bearer_prefix: Option<&str>);
}

// ============================================================================
// Retry Configuration
// ============================================================================

/// Configuration for exponential backoff retry logic.
///
/// This is particularly important for handling 429 (rate limit) errors from GCS,
/// which commonly occur during autoscaling events.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Initial delay before the first retry (default: 500ms)
    initial_delay: Duration,
    /// Maximum delay between retries (default: 30s)
    max_delay: Duration,
    /// Maximum number of retry attempts (default: 5)
    max_retries: u32,
    /// Multiplier for exponential backoff (default: 2.0)
    multiplier: f64,
    /// Jitter factor (0.0 to 1.0) - randomizes delay to prevent thundering herd (default: 0.5)
    jitter_factor: f64,
    /// Whether to respect Retry-After headers from 429 responses (default: true)
    respect_retry_after: bool,
    /// Maximum delay to honor from Retry-After header (default: 60s)
    max_retry_after: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            max_retries: 5,
            multiplier: 2.0,
            jitter_factor: 0.5,
            respect_retry_after: true,
            max_retry_after: Duration::from_secs(60),
        }
    }
}

impl RetryConfig {
    /// Create a new RetryConfig with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a conservative config for high-reliability scenarios.
    /// Uses longer delays and more retries.
    pub fn conservative() -> Self {
        Self {
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            max_retries: 8,
            multiplier: 2.0,
            jitter_factor: 0.5,
            respect_retry_after: true,
            max_retry_after: Duration::from_secs(120),
        }
    }

    /// Set the initial delay before the first retry.
    pub fn with_initial_delay(mut self, delay: Duration) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Set the maximum delay between retries.
    pub fn with_max_delay(mut self, delay: Duration) -> Self {
        self.max_delay = delay;
        self
    }

    /// Set the maximum number of retry attempts.
    pub fn with_max_retries(mut self, retries: u32) -> Self {
        self.max_retries = retries;
        self
    }

    /// Set the exponential backoff multiplier.
    pub fn with_multiplier(mut self, multiplier: f64) -> Self {
        self.multiplier = multiplier;
        self
    }

    /// Set the jitter factor (0.0 to 1.0).
    pub fn with_jitter_factor(mut self, factor: f64) -> Self {
        self.jitter_factor = factor.clamp(0.0, 1.0);
        self
    }

    /// Calculate the delay for a given attempt number (0-indexed).
    /// Includes jitter to prevent thundering herd.
    fn calculate_delay(&self, attempt: u32) -> Duration {
        let base_delay_ms =
            self.initial_delay.as_millis() as f64 * self.multiplier.powi(attempt as i32);
        let capped_delay_ms = base_delay_ms.min(self.max_delay.as_millis() as f64);

        // Apply jitter using a simple hash of the current time
        // This provides enough randomness to prevent thundering herd without needing rand
        let jitter_value = generate_jitter(self.jitter_factor);
        let jittered_delay_ms = capped_delay_ms * (1.0 + jitter_value);

        Duration::from_millis(jittered_delay_ms.max(0.0) as u64)
    }

    /// Calculate delay considering a Retry-After header value.
    fn calculate_delay_with_retry_after(
        &self,
        attempt: u32,
        retry_after: Option<Duration>,
    ) -> Duration {
        let base_delay = self.calculate_delay(attempt);

        if self.respect_retry_after
            && let Some(server_delay) = retry_after
        {
            // Use the larger of our calculated delay or server's Retry-After,
            // but cap at max_retry_after to prevent abuse
            let capped_server_delay = server_delay.min(self.max_retry_after);
            return base_delay.max(capped_server_delay);
        }

        base_delay
    }
}

/// HTTP upload error with structured status code.
#[derive(Debug)]
pub struct HttpUploadError {
    pub status_code: u16,
    pub message: String,
}

impl std::fmt::Display for HttpUploadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for HttpUploadError {}

/// Result of checking if an HTTP response should be retried.
struct ResponseCheck {
    /// Whether the error is retryable (edge rule — see `is_retryable_status`).
    pub is_retryable: bool,
    /// Retry-After header value if present.
    pub retry_after: Option<Duration>,
    pub status_code: u16,
    pub message: String,
}

impl ResponseCheck {
    /// Check an HTTP response and determine retry behavior.
    async fn from_response(response: reqwest::Response, operation: &str) -> Self {
        let status = response.status();
        let status_code = status.as_u16();
        let retry_after = parse_retry_after(&response);
        let error_body = response.text().await.unwrap_or_default();

        Self {
            is_retryable: is_retryable_status(status_code),
            retry_after,
            status_code,
            message: format!("{}: HTTP {} - {}", operation, status, error_body),
        }
    }

    /// Log a retry warning and sleep for the appropriate delay.
    async fn wait_for_retry(&self, retry_config: &RetryConfig, attempt: u32) {
        let delay = retry_config.calculate_delay_with_retry_after(attempt, self.retry_after);
        tracing::warn!(
            "{} (attempt {}/{}), retrying in {:?}{}",
            self.message,
            attempt + 1,
            retry_config.max_retries,
            delay,
            self.retry_after
                .map_or(String::new(), |d| format!(" (Retry-After: {:?})", d))
        );
        tokio::time::sleep(delay).await;
    }
}

/// Log a network error and sleep for retry.
async fn wait_for_network_retry(
    retry_config: &RetryConfig,
    operation: &str,
    attempt: u32,
    error: &(dyn std::fmt::Display + Send + Sync),
) {
    let delay = retry_config.calculate_delay(attempt);
    tracing::warn!(
        "{} request failed (attempt {}/{}), retrying in {:?}: {}",
        operation,
        attempt + 1,
        retry_config.max_retries,
        delay,
        error
    );
    tokio::time::sleep(delay).await;
}

/// Generate a jitter value in the range [-jitter_factor/2, +jitter_factor/2].
/// Uses the current timestamp as a source of randomness - sufficient for preventing
/// thundering herd without requiring a full random number generator.
fn generate_jitter(jitter_factor: f64) -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Use nanoseconds from current time as pseudo-random source
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);

    // Mix the bits a bit using a simple hash
    let mixed = ((nanos as u64).wrapping_mul(0x517cc1b727220a95) >> 32) as u32;

    // Convert to a value in [0, 1)
    let random_01 = (mixed as f64) / (u32::MAX as f64);

    // Map to [-jitter_factor/2, +jitter_factor/2]
    (random_01 * jitter_factor) - (jitter_factor / 2.0)
}

/// Outcome of a storage existence check. `ProbeFailed` (transient: network /
/// parse / 5xx) is distinct from `NotFound` so callers don't synthesise a
/// phantom-missing answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExistsResult<T> {
    Found(T),
    NotFound,
    Unauthorized,
    ProbeFailed,
}

/// Response from uploading to GCS.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UploadResponse {
    pub bucket: String,
    pub path: String,
    pub size: i64,
    pub content_type: String,
    pub generation: i64,
}

/// Response from GET /v1/storage/limits endpoint (storage-related config).
/// 0 = unlimited (backward compatible default).
#[derive(Debug, Clone, Deserialize)]
pub struct UploadLimits {
    /// Max bytes for inlining file content in SerializeRepoChangesRequest (e.g. untracked files, binaries).
    pub max_file_bytes: u64,
    /// Max for untracked files specifically (may differ in future).
    pub max_untracked_bytes: u64,
    /// Whether limits are enforced.
    pub enabled: bool,
}

/// Parse the Retry-After header value into a Duration.
/// Supports delay-seconds format (most common from GCS).
fn parse_retry_after(response: &reqwest::Response) -> Option<Duration> {
    let header_value = response.headers().get("retry-after")?;
    let header_str = header_value.to_str().ok()?;

    // Parse as seconds (most common format from GCS and other APIs)
    if let Ok(seconds) = header_str.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    // Note: HTTP-date format (e.g., "Wed, 21 Oct 2015 07:28:00 GMT") is not supported
    // as it's rarely used by GCS and would require additional parsing complexity

    None
}

/// Whether an HTTP status is retryable. Proxy-mode uploads cross the same
/// Cloudflare edge as sampling, so they share its rule (429 + 5xx, minus
/// origin-TLS 525/526).
#[inline]
fn is_retryable_status(status: u16) -> bool {
    RetryPolicy::edge_client().should_retry(status)
}

#[cfg(test)]
mod retry_status_tests {
    use super::is_retryable_status;

    /// Pins the upload path to the edge rule; the exhaustive per-status
    /// matrix lives in `echo-agent-circuit-breaker`'s `edge_client` tests.
    #[test]
    fn uploads_use_the_edge_retry_rule() {
        assert!(is_retryable_status(522));
        assert!(!is_retryable_status(525));
        assert!(!is_retryable_status(413));
    }
}

/// Static credentials for the proxy-mode upload path when no `AuthManager`
/// is available (bins, tests, bare `TraceExportConfig` with no auth wrap).
///
/// SAFETY: only hit by bins/tests/no-AuthManager paths; the refresh-aware
/// path uses the obfuscated shell impl (`EchoAgentAuthCredentials::apply` via
/// `ShellAuthCredentialProvider`). A future reader should not innocently
/// make the static provider the production default -- the obfuscated
/// routing + obfstr-protected literals only live in the shell impl.
pub struct StaticEchoAgentAuth {
    pub user_token: Option<String>,
    pub deployment_key: Option<String>,
}

impl StaticEchoAgentAuth {
    pub fn new(user_token: Option<String>) -> Self {
        Self {
            user_token,
            deployment_key: None,
        }
    }

    /// Returns the bearer that `apply` will put on the wire (deployment_key
    /// first, else user_token). Used by callers that need to wire the same
    /// bearer into a `StaticAuthCredentialProvider` snapshot for attribution.
    pub fn wire_bearer(&self) -> Option<String> {
        self.deployment_key
            .clone()
            .or_else(|| self.user_token.clone())
    }
}

impl echo_agent_auth::HttpAuth for StaticEchoAgentAuth {
    fn apply(&self, builder: reqwest::RequestBuilder, _base_url: &str) -> reqwest::RequestBuilder {
        if let Some(ref key) = self.deployment_key {
            builder.header("Authorization", format!("Bearer {}", key))
        } else if let Some(ref token) = self.user_token {
            builder
                .header("Authorization", format!("Bearer {}", token))
                .header("X-Echo-Agent-Token-Auth", "echo-agent-cli")
        } else {
            builder
        }
    }
}

#[cfg(test)]
mod static_echo_agent_auth_tests {
    use super::StaticEchoAgentAuth;

    /// Deployment key must win over the user token (incl. the empty one the
    /// deployment-key path supplies); falls back to the user token otherwise.
    #[test]
    fn wire_bearer_prefers_deployment_key_then_falls_back_to_user_token() {
        let mut deployment = StaticEchoAgentAuth::new(Some(String::new()));
        deployment.deployment_key = Some("deploy-key".to_string());
        assert_eq!(deployment.wire_bearer().as_deref(), Some("deploy-key"));

        let oauth = StaticEchoAgentAuth::new(Some("oauth-token".to_string()));
        assert_eq!(oauth.wire_bearer().as_deref(), Some("oauth-token"));
    }
}

/// Default reqwest client used by `StorageClient::new`. Plain defaults --
/// production callers should instead pass a tuned client (e.g. shell's
/// `crate::http::shared_upload_client()`) to `with_provider`.
fn default_upload_client() -> Client {
    #[expect(clippy::expect_used)]
    echo_agent_extra_ca::build_reqwest_client(|builder| builder)
        .expect("default reqwest client builds")
}

/// Client for uploading files to GCS via cli-chat-proxy.
#[derive(Clone)]
pub struct StorageClient {
    http_client: reqwest_middleware::ClientWithMiddleware,
    /// Plain `reqwest::Client` for requests that must NOT go through the
    /// auth middleware (direct GCS uploads via signed URLs, signed-URL
    /// downloads, etc.).
    raw_http_client: Client,
    /// Base URL for the proxy (e.g., "https://chat-proxy.echo-agent.invalid/v1")
    base_url: String,
    /// Retry configuration for handling transient failures (especially 429 errors)
    retry_config: RetryConfig,
    /// Optional callback invoked on every 401 so the embedding application
    /// can record auth-attribution telemetry. Shell installs a bridge here;
    /// bins/tests typically leave it `None`.
    attribution: Option<Arc<dyn Auth401AttributionCallback>>,
    /// Credential provider used to snapshot the bearer prefix at 401 sites
    /// for attribution telemetry.
    credentials: Arc<dyn AuthCredentialProvider>,

    /// Client identity forwarded to cli-chat-proxy (for logging + metrics).
    /// Set via `with_client_identity` / `with_client_mode`.
    client_version: Option<String>,
    client_identifier: Option<String>,
    client_mode: Option<String>,

    /// Session-wide breaker, shared across `Clone`s.
    breaker: CircuitBreaker,
}

impl StorageClient {
    /// Creates a new StorageClient with a static user-token credential.
    ///
    /// Convenience constructor for bins, tests, and other callers that have a
    /// raw bearer string and no AuthManager. Internally wraps the token in a
    /// `StaticAuthCredentialProvider` -- there is no refresh capability and no
    /// attribution callback.
    ///
    /// Production code with refresh-aware auth should use [`Self::with_provider`].
    ///
    /// # Arguments
    /// * `proxy_base_url` - Base URL for the proxy (e.g., "https://chat-proxy.echo-agent.invalid/v1")
    /// * `user_token` - User's cloud.echo-agent.invalid auth token
    pub fn new(proxy_base_url: &str, user_token: &str) -> Self {
        let creds = StaticEchoAgentAuth::new(Some(user_token.to_owned()));
        let bearer = creds.wire_bearer();
        let provider = Arc::new(echo_agent_auth::StaticAuthCredentialProvider::new(
            Box::new(creds),
            bearer,
        ));
        Self::with_provider(proxy_base_url, default_upload_client(), provider)
    }

    /// Creates a new StorageClient with an explicit HTTP client and credential
    /// provider. The caller controls the user-agent / connection-pool tuning of
    /// the HTTP client (shell passes `crate::http::shared_upload_client()`).
    pub fn with_provider(
        proxy_base_url: &str,
        http_client: Client,
        credentials: Arc<dyn AuthCredentialProvider>,
    ) -> Self {
        let middleware_client = crate::with_auth_retry(http_client.clone(), credentials.clone());
        let breaker = CircuitBreaker::new(storage_breaker_config())
            .with_observer(TracingObserver::new(STORAGE_BREAKER_NAME));
        Self {
            http_client: middleware_client,
            raw_http_client: http_client,
            base_url: proxy_base_url.to_owned(),
            retry_config: RetryConfig::default(),
            attribution: None,
            credentials,
            client_version: None,
            client_identifier: None,
            client_mode: None,
            breaker,
        }
    }

    /// Returns `true` if the storage circuit breaker is currently open.
    pub fn storage_breaker_is_open(&self) -> bool {
        self.breaker.is_open()
    }

    /// Test-only constructor that rebuilds the breaker with a shortened
    /// `open_duration` so cool-down windows finish in tens of
    /// milliseconds. Preserves observer fidelity by reinstalling the
    /// production `TracingObserver`.
    ///
    /// **Observer-clobber**: this replaces the whole breaker (and its
    /// observer) wholesale. Tests that want a custom observer must use
    /// [`Self::with_breaker_for_testing`] instead. If a future feature
    /// exposes an open-duration knob in non-test code it must NOT use
    /// this helper; add a dedicated setter on the shared crate so the
    /// existing observer is preserved.
    #[cfg(test)]
    fn with_breaker_open_duration(mut self, open_duration: Duration) -> Self {
        let mut config = storage_breaker_config();
        config.open_duration = open_duration;
        self.breaker =
            CircuitBreaker::new(config).with_observer(TracingObserver::new(STORAGE_BREAKER_NAME));
        self
    }

    /// Test-only constructor that rebuilds the breaker with a shortened
    /// `open_duration` AND a caller-supplied observer.
    ///
    /// **Observer-clobber**: same caveat as
    /// [`Self::with_breaker_open_duration`] — the supplied observer
    /// fully replaces the production `TracingObserver`.
    #[cfg(test)]
    fn with_breaker_for_testing(
        mut self,
        open_duration: Duration,
        observer: std::sync::Arc<dyn echo_agent_circuit_breaker::Observer>,
    ) -> Self {
        let mut config = storage_breaker_config();
        config.open_duration = open_duration;
        self.breaker = CircuitBreaker::new(config).with_observer(observer);
        self
    }

    /// Install a 401-attribution callback. Shell uses this to wire
    /// `record_consumer_401`; bins/tests typically omit it.
    pub fn with_attribution(mut self, callback: Arc<dyn Auth401AttributionCallback>) -> Self {
        self.attribution = Some(callback);
        self
    }

    /// Configures the client identity reported to the storage backend on all
    /// storage requests (including the high-traffic `batch_upload`).
    ///
    /// These become the headers:
    ///   - `x-echo-agent-client-version`
    ///   - `x-echo-agent-client-identifier` (one of "echo-agent-shell", "echo-agent-pager",
    ///     "echo-agent-desktop", "echo-agent-extension", "echo-agent-agent-sdk")
    ///
    /// Server-side logs in `cli-chat-proxy` and analytics queries now
    /// surface these values, making it easy to attribute 400/403 errors to
    /// specific client versions and products.
    ///
    /// Preferred way to construct the client from the EchoAgent shell/pager:
    ///   `build_storage_client_for_proxy(..., client_identifier)`
    /// (see `echo-agent-runtime/src/auth/credential_provider.rs`).
    ///
    /// Direct callers (tests, load-test binaries, etc.) can use:
    ///   `StorageClient::with_provider(...).with_client_identity(version, identifier)`
    pub fn with_client_identity(
        mut self,
        version: impl Into<String>,
        identifier: impl Into<String>,
    ) -> Self {
        self.client_version = Some(version.into());
        self.client_identifier = Some(identifier.into());
        self
    }

    /// Sets the `x-echo-agent-client-mode` value forwarded to cli-chat-proxy
    /// (`headless` / `interactive`), for the `client_mode` metric label.
    pub fn with_client_mode(mut self, mode: impl Into<String>) -> Self {
        self.client_mode = Some(mode.into());
        self
    }

    /// Sets the retry configuration for handling transient failures.
    ///
    /// This is particularly useful for tuning retry behavior for 429 (rate limit) errors
    /// that commonly occur during GCS autoscaling events.
    ///
    /// # Example
    /// ```rust,ignore
    /// let client = StorageClient::new(url, token)
    ///     .with_retry_config(RetryConfig::conservative());
    /// ```
    pub fn with_retry_config(mut self, config: RetryConfig) -> Self {
        self.retry_config = config;
        self
    }

    /// Fetches upload size limits from cli-chat-proxy.
    /// GET /v1/storage/limits
    ///
    /// Reuses `add_common_headers` for the shared client headers. Returns UploadLimits
    /// struct; 0 = unlimited.
    pub async fn get_upload_limits(&self) -> Result<UploadLimits> {
        let url = format!("{}/storage/limits", self.base_url);
        let request = self.add_common_headers(self.http_client.get(&url));

        let response = request.send().await.context("Fetching upload limits")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            anyhow::bail!(
                "Upload limits fetch failed with status {}: {}",
                status,
                body
            );
        }

        response
            .json()
            .await
            .context("Failed to parse upload limits response")
    }

    /// Fire the attribution callback if installed.
    ///
    /// `operation` is the consumer-side op *suffix* (e.g. `"check_exists"`,
    /// `"batch_check_exists"`, `"upload"`, `"upload_file"`). The
    /// `StorageClientAttributionBridge` in the shell prepends
    /// `"StorageClient."` to produce the final consumer string in analytics
    /// events (e.g. `"StorageClient.check_exists"`).
    ///
    /// Reads the bearer from the credential provider's current snapshot
    /// (not the exact wire bearer — a refresh may have occurred between
    /// send and 401 response, though in practice this is rare).
    fn fire_401_attribution(&self, operation: &str) {
        if let Some(ref cb) = self.attribution {
            let bearer_prefix = self.credentials.snapshot().token;
            cb.record_401(operation, bearer_prefix.as_deref());
        }
    }

    /// GET /v1/storage/exists. 2xx → `Found`; 401/403 → `Unauthorized`;
    /// 404 → `NotFound`; everything else → `ProbeFailed`.
    pub async fn check_exists(&self, path: &str) -> ExistsResult<UploadResponse> {
        if self.breaker.check().is_err() {
            return ExistsResult::Unauthorized;
        }
        let url = format!("{}/storage/exists", self.base_url);
        let request = self
            .add_common_headers(self.http_client.get(&url))
            .header("X-Storage-Path", path);

        match request.send().await {
            Ok(resp) if resp.status().is_success() => {
                // Defer the Success record until after the body
                // parses — a 2xx with an unparseable body returns
                // `ProbeFailed` to the caller, so counting it as
                // a breaker Success would dilute the failure rate
                // during partial-outage / bad-payload scenarios.
                match resp.json().await {
                    Ok(payload) => {
                        self.breaker.record(Outcome::Success);
                        ExistsResult::Found(payload)
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse check_exists response: {e}");
                        ExistsResult::ProbeFailed
                    }
                }
            }
            Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                self.fire_401_attribution("check_exists");
                self.breaker.record(Outcome::Failure);
                ExistsResult::Unauthorized
            }
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => ExistsResult::NotFound,
            // 403 fires attribution but, per the breaker contract, does
            // NOT count toward the 401 counter.
            Ok(resp) if resp.status() == reqwest::StatusCode::FORBIDDEN => {
                self.fire_401_attribution("check_exists");
                ExistsResult::Unauthorized
            }
            Ok(resp) => {
                tracing::warn!("check_exists returned {}", resp.status());
                ExistsResult::ProbeFailed
            }
            Err(e) => {
                tracing::warn!("check_exists request failed: {e}");
                ExistsResult::ProbeFailed
            }
        }
    }

    /// POST /v1/storage/batch_exists. Same status mapping as `check_exists`,
    /// returning the set of paths confirmed present.
    ///
    /// Accepts any `AsRef<str>` slice so callers can pass `&[String]`, `&[&str]`,
    /// or `&[Cow<'_, str>]` without cloning into a fresh `Vec<String>`.
    pub async fn batch_check_exists<S: AsRef<str>>(
        &self,
        paths: &[S],
    ) -> ExistsResult<HashSet<String>> {
        #[derive(Serialize)]
        struct Request<'a> {
            paths: Vec<&'a str>,
        }

        #[derive(Deserialize)]
        struct Response {
            exists: Vec<String>,
        }

        // Collect into &str up front so the JSON body holds borrows tied to
        // `paths` rather than the generic `S`, avoiding HRTB issues at callers.
        let paths_ref: Vec<&str> = paths.iter().map(<S as AsRef<str>>::as_ref).collect();
        if self.breaker.check().is_err() {
            return ExistsResult::Unauthorized;
        }
        let url = format!("{}/storage/batch_exists", self.base_url);
        let request = self
            .add_common_headers(self.http_client.post(&url))
            .json(&Request { paths: paths_ref });

        match request.send().await {
            Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                self.fire_401_attribution("batch_check_exists");
                self.breaker.record(Outcome::Failure);
                ExistsResult::Unauthorized
            }
            Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => ExistsResult::NotFound,
            // 403 fires attribution; does NOT count toward the breaker.
            Ok(resp) if resp.status() == reqwest::StatusCode::FORBIDDEN => {
                self.fire_401_attribution("batch_check_exists");
                ExistsResult::Unauthorized
            }
            Ok(resp) if resp.status().is_success() => {
                // Defer the Success record until after parse — see
                // the matching comment in `check_exists`. A 2xx
                // with an unparseable body returns `ProbeFailed`,
                // which the caller treats as transient; counting
                // it as a breaker Success masks proxy / payload
                // outages.
                match resp.json::<Response>().await {
                    Ok(r) => {
                        self.breaker.record(Outcome::Success);
                        ExistsResult::Found(r.exists.into_iter().collect())
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse batch_exists response: {e}");
                        ExistsResult::ProbeFailed
                    }
                }
            }
            Ok(resp) => {
                tracing::warn!("batch_exists returned {}", resp.status());
                ExistsResult::ProbeFailed
            }
            Err(e) => {
                tracing::warn!("batch_exists request failed: {e}");
                ExistsResult::ProbeFailed
            }
        }
    }

    /// Upload multiple small files in a single batch request.
    /// POST /v1/storage/batch_upload (multipart/form-data)
    ///
    /// Returns `Some(results)` on success, or `None` if the endpoint is
    /// unavailable (404 from old proxy) or on error. Callers should fall back
    /// to per-file `upload` when `None` is returned.
    pub async fn batch_upload(
        &self,
        files: Vec<(String, Vec<u8>, String)>,
    ) -> Option<Vec<echo_agent_chat_proxy_types::BatchUploadResult>> {
        let url = format!("{}/storage/batch_upload", self.base_url);
        let operation = "batch_upload";

        if self.breaker.check().is_err() {
            return None;
        }

        let mut attempt = 0u32;
        loop {
            // Rebuild the multipart form on each attempt (consumed by send).
            let mut form = reqwest::multipart::Form::new();
            for (path, content, content_type) in &files {
                let ct = if content_type.is_empty() {
                    "application/octet-stream"
                } else {
                    content_type.as_str()
                };
                let part = match reqwest::multipart::Part::bytes(content.clone()).mime_str(ct) {
                    Ok(part) => part.file_name(path.clone()),
                    Err(e) => {
                        tracing::warn!("batch_upload: invalid MIME type '{ct}': {e}");
                        return None;
                    }
                };
                form = form.part(path.clone(), part);
            }

            let request = self
                .add_common_headers(self.http_client.post(&url))
                .multipart(form);

            match request.send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    self.fire_401_attribution("batch_upload");
                    self.breaker.record(Outcome::Failure);
                    return None;
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => return None,
                Ok(resp) if resp.status() == reqwest::StatusCode::FORBIDDEN => {
                    tracing::debug!("batch_upload rejected (403), skipping");
                    return None;
                }
                // 422: server detected the body was stripped in transit
                // (Content-Length: 0). Retry with exponential backoff.
                Ok(resp) if resp.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
                    if attempt < self.retry_config.max_retries {
                        wait_for_network_retry(
                            &self.retry_config,
                            "batch_upload (body stripped in transit)",
                            attempt,
                            &"422 Unprocessable Entity",
                        )
                        .await;
                        attempt += 1;
                        continue;
                    }
                    tracing::warn!(
                        "batch_upload: body stripped in transit (422), giving up after retries"
                    );
                    return None;
                }
                Ok(resp) if resp.status().is_success() => {
                    self.breaker.record(Outcome::Success);
                    return match resp
                        .json::<echo_agent_chat_proxy_types::BatchUploadResponse>()
                        .await
                    {
                        Ok(r) => Some(r.results),
                        Err(e) => {
                            tracing::warn!("Failed to parse batch_upload response: {e}");
                            None
                        }
                    };
                }
                Ok(resp) => {
                    let check = ResponseCheck::from_response(resp, operation).await;
                    if check.is_retryable && attempt < self.retry_config.max_retries {
                        check.wait_for_retry(&self.retry_config, attempt).await;
                        attempt += 1;
                        continue;
                    }
                    tracing::warn!("batch_upload failed: {}", check.message);
                    return None;
                }
                Err(e) => {
                    if attempt < self.retry_config.max_retries {
                        wait_for_network_retry(&self.retry_config, operation, attempt, &e).await;
                        attempt += 1;
                        continue;
                    }
                    tracing::warn!("batch_upload request failed after retries: {e}");
                    return None;
                }
            }
        }
    }

    /// Upload multiple small files in a single JSON request with zstd compression.
    /// POST /v1/storage/batch_upload_json
    ///
    /// Encodes file contents as base64 in a JSON body, compresses with zstd,
    /// and sends with `Content-Encoding: zstd`. The payload is a plain `Vec<u8>`
    /// (compressed JSON), so retries just re-send the same bytes without
    /// rebuilding a multipart form.
    ///
    /// If `files` is empty, this is a no-op that returns `Some(Vec::new())`
    /// without making any HTTP request.
    ///
    /// Returns `Some(results)` on success, or `None` if the endpoint is
    /// unavailable (404 from old proxy) or on error.
    pub async fn batch_upload_json(
        &self,
        files: Vec<(String, Vec<u8>, String)>,
    ) -> Option<Vec<echo_agent_chat_proxy_types::BatchUploadResult>> {
        // Short-circuit on empty input: the server rejects empty payloads with
        // a 400, and there are no per-file results to return anyway.
        if files.is_empty() {
            tracing::debug!("batch_upload_json: skipping empty file list");
            return Some(Vec::new());
        }

        use base64::Engine;
        let b64_engine = base64::engine::general_purpose::STANDARD;

        let request_body = echo_agent_chat_proxy_types::BatchUploadRequest {
            files: files
                .into_iter()
                .map(|(path, content, content_type)| {
                    let data = b64_engine.encode(&content);
                    echo_agent_chat_proxy_types::BatchUploadFile {
                        path,
                        content_type: if content_type.is_empty() {
                            "application/octet-stream".to_string()
                        } else {
                            content_type
                        },
                        data,
                    }
                })
                .collect(),
        };

        let json_bytes = match serde_json::to_vec(&request_body) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("batch_upload_json: failed to serialize request: {e}");
                return None;
            }
        };

        let compressed = match zstd::encode_all(std::io::Cursor::new(&json_bytes), 3) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("batch_upload_json: zstd compression failed: {e}");
                return None;
            }
        };
        drop(json_bytes);
        drop(request_body);

        let url = format!("{}/storage/batch_upload_json", self.base_url);
        let operation = "batch_upload_json";

        if self.breaker.check().is_err() {
            return None;
        }

        let mut attempt = 0u32;
        loop {
            let request = self
                .add_common_headers(self.http_client.post(&url))
                .header("Content-Type", "application/json")
                .header("Content-Encoding", "zstd")
                // Clone needed: reqwest consumes the body on send, but we
                // need it intact for retries.
                .body(compressed.clone());

            match request.send().await {
                Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    self.fire_401_attribution("batch_upload_json");
                    self.breaker.record(Outcome::Failure);
                    return None;
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => return None,
                Ok(resp) if resp.status() == reqwest::StatusCode::FORBIDDEN => {
                    tracing::debug!("batch_upload_json rejected (403), skipping");
                    return None;
                }
                Ok(resp) if resp.status().is_success() => {
                    self.breaker.record(Outcome::Success);
                    return match resp
                        .json::<echo_agent_chat_proxy_types::BatchUploadResponse>()
                        .await
                    {
                        Ok(r) => Some(r.results),
                        Err(e) => {
                            tracing::warn!("Failed to parse batch_upload_json response: {e}");
                            None
                        }
                    };
                }
                Ok(resp) => {
                    let check = ResponseCheck::from_response(resp, operation).await;
                    if check.is_retryable && attempt < self.retry_config.max_retries {
                        check.wait_for_retry(&self.retry_config, attempt).await;
                        attempt += 1;
                        continue;
                    }
                    tracing::warn!("batch_upload_json failed: {}", check.message);
                    return None;
                }
                Err(e) => {
                    if attempt < self.retry_config.max_retries {
                        wait_for_network_retry(&self.retry_config, operation, attempt, &e).await;
                        attempt += 1;
                        continue;
                    }
                    tracing::warn!("batch_upload_json request failed after retries: {e}");
                    return None;
                }
            }
        }
    }

    /// Download a dedup blob from GCS to a local destination path.
    /// GET /v1/storage/download
    ///
    /// Authorization is enforced by the proxy. The proxy generates a short-lived
    /// signed GET URL; this method follows that URL and streams the object bytes
    /// to `dest`.
    ///
    /// Returns `Err` with a clear message on 403 (not authorized) or any other
    /// non-success response.
    ///
    /// This is the download primitive for deduped content materialisation during
    /// restore. Callers should only invoke it when download is permitted for the
    /// current session.
    pub async fn download_blob(&self, storage_path: &str, dest: &Path) -> Result<()> {
        // Step 1: get a signed GET URL from the proxy.
        let url = format!("{}/storage/download", self.base_url);
        let resp = self
            .add_common_headers(self.http_client.get(&url))
            .header("X-Storage-Path", storage_path)
            .send()
            .await
            .context("Failed to contact storage download endpoint")?;

        if resp.status() == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!(
                "Dedup blob download is not authorized for this account \
                 (server returned 403 for path '{}')",
                storage_path
            );
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Storage download endpoint returned {status} for '{}': {body}",
                storage_path
            );
        }

        #[derive(serde::Deserialize)]
        struct DownloadResponse {
            signed_url: String,
        }

        let download_resp: DownloadResponse = resp
            .json()
            .await
            .context("Failed to parse storage download response")?;

        // Step 2: fetch the object via the signed URL, streaming to dest.
        // Use the raw client — signed URLs carry their own auth and must
        // NOT go through the AuthRetryMiddleware.
        let object_resp = self
            .raw_http_client
            .get(&download_resp.signed_url)
            .send()
            .await
            .context("Failed to fetch blob from signed URL")?;

        if !object_resp.status().is_success() {
            let status = object_resp.status();
            let body = object_resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "Signed URL download failed with {status} for '{}': {body}",
                storage_path
            );
        }

        // Create parent directories if needed.
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent dirs for {}", dest.display()))?;
        }

        // Stream response body directly to disk — avoids buffering the whole
        // blob in memory, which matters for large binary referenced content.
        let mut file = tokio::fs::File::create(dest)
            .await
            .with_context(|| format!("Failed to create destination file {}", dest.display()))?;
        let mut stream = object_resp.bytes_stream();
        use futures::StreamExt as _;
        use tokio::io::AsyncWriteExt as _;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk
                .with_context(|| format!("Error reading blob stream for '{}'", storage_path))?;
            file.write_all(&chunk)
                .await
                .with_context(|| format!("Failed to write chunk to {}", dest.display()))?;
        }
        file.flush()
            .await
            .with_context(|| format!("Failed to flush {}", dest.display()))?;

        Ok(())
    }

    fn add_common_headers(
        &self,
        builder: reqwest_middleware::RequestBuilder,
    ) -> reqwest_middleware::RequestBuilder {
        // Prefer caller-provided identity (from shell/pager/etc.) so that
        // cli-chat-proxy logs and metrics see the real end-user client
        // (e.g. "0.1.210-alpha.5", "echo-agent-shell" / "echo-agent-pager").
        // Falls back to the library's own version for bins/tests.
        let version = self
            .client_version
            .as_deref()
            .unwrap_or(echo_agent_version::VERSION);
        let mut builder = builder.header("x-echo-agent-client-version", version);

        if let Some(id) = &self.client_identifier {
            builder = builder.header("x-echo-agent-client-identifier", id);
        }

        if let Some(mode) = &self.client_mode {
            builder = builder.header("x-echo-agent-client-mode", mode);
        }

        for (name, value) in crate::trace_context::trace_context_headers().iter() {
            builder = builder.header(name.clone(), value.clone());
        }
        builder
    }

    /// Uploads content to GCS with retry logic for transient failures.
    ///
    /// Retries on 429 (rate limit), 500, 502, 503, 504 errors with configurable
    /// exponential backoff and jitter. Respects `Retry-After` headers from 429 responses.
    ///
    /// The bucket is determined by the proxy based on the user's ACLs.
    ///
    /// # Arguments
    /// * `path` - The destination path in the bucket (e.g., "uploads/file.txt")
    /// * `content` - The content to upload as bytes
    /// * `content_type` - MIME type of the content (e.g., "text/plain", "application/octet-stream")
    ///
    /// # Returns
    /// The upload response containing bucket, path, size, content_type, and generation.
    pub async fn upload(
        &self,
        path: &str,
        content: &[u8],
        content_type: &str,
    ) -> Result<UploadResponse> {
        let url = format!("{}/storage", self.base_url);
        let content = content.to_vec();
        let operation = format!("Upload to '{}'", path);

        if let Err(BreakerOpen { retry_after }) = self.breaker.check() {
            return Err(HttpUploadError {
                status_code: 503,
                message: format!(
                    "upload: circuit breaker open; retry after {:.1}s",
                    retry_after.as_secs_f64()
                ),
            }
            .into());
        }

        let mut attempt = 0u32;
        loop {
            // Re-consult the breaker at the top of each retry
            // iteration. The initial check above gates entry, but
            // 429/5xx retries can continue issuing requests after
            // a concurrent call has tripped the breaker open;
            // re-checking preserves the "no wire I/O while open"
            // invariant matching the half-open probe gate.
            if attempt > 0
                && let Err(BreakerOpen { retry_after }) = self.breaker.check()
            {
                return Err(HttpUploadError {
                    status_code: 503,
                    message: format!(
                        "upload: circuit breaker opened during retries; retry after {:.1}s",
                        retry_after.as_secs_f64()
                    ),
                }
                .into());
            }

            let request = self.add_common_headers(
                self.http_client
                    .post(&url)
                    .header("Content-Type", content_type)
                    .header("X-Storage-Path", path),
            );

            match request.body(content.clone()).send().await {
                Ok(response) if response.status().is_success() => {
                    self.breaker.record(Outcome::Success);
                    return response
                        .json()
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to parse upload response: {}", e));
                }
                Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    self.fire_401_attribution("upload");
                    self.breaker.record(Outcome::Failure);
                    return Err(HttpUploadError {
                        status_code: 401,
                        message: format!("{}: HTTP 401 Unauthorized", operation),
                    }
                    .into());
                }
                Ok(response) if response.status() == reqwest::StatusCode::FORBIDDEN => {
                    let body = response.text().await.unwrap_or_default();
                    tracing::warn!("storage upload rejected (403): {body}");
                    return Err(HttpUploadError {
                        status_code: 403,
                        message: format!("{operation}: HTTP 403 Forbidden - {body}"),
                    }
                    .into());
                }
                Ok(response) => {
                    let check = ResponseCheck::from_response(response, &operation).await;
                    if check.is_retryable && attempt < self.retry_config.max_retries {
                        check.wait_for_retry(&self.retry_config, attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(HttpUploadError {
                        status_code: check.status_code,
                        message: check.message,
                    }
                    .into());
                }
                Err(e) => {
                    if attempt < self.retry_config.max_retries {
                        wait_for_network_retry(&self.retry_config, &operation, attempt, &e).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(anyhow::anyhow!("{} failed: {}", operation, e));
                }
            }
        }
    }

    /// Uploads a file to GCS with retry logic for transient failures.
    ///
    /// This is more memory-efficient than `upload()` as it streams the file content
    /// without loading it entirely into memory. Unlike `upload_stream()`, this method
    /// supports automatic retries by re-reading from the file on each attempt.
    ///
    /// Retries on 429 (rate limit), 500, 502, 503, 504 errors with configurable
    /// exponential backoff and jitter. Respects `Retry-After` headers from 429 responses.
    ///
    /// # Arguments
    /// * `dest_path` - The destination path in the bucket (e.g., "uploads/file.txt")
    /// * `file_path` - Path to the local file to upload
    /// * `content_type` - MIME type of the content (e.g., "application/gzip")
    ///
    /// # Returns
    /// The upload response containing bucket, path, size, content_type, and generation.
    pub async fn upload_file(
        &self,
        dest_path: &str,
        file_path: &Path,
        content_type: &str,
    ) -> Result<UploadResponse> {
        let url = format!("{}/storage", self.base_url);
        let operation = format!("Upload file '{}'", file_path.display());

        if let Err(BreakerOpen { retry_after }) = self.breaker.check() {
            return Err(HttpUploadError {
                status_code: 503,
                message: format!(
                    "upload_file: circuit breaker open; retry after {:.1}s",
                    retry_after.as_secs_f64()
                ),
            }
            .into());
        }

        let metadata = std::fs::metadata(file_path)
            .with_context(|| format!("Failed to get file metadata: {}", file_path.display()))?;
        let file_size = metadata.len();

        let file = StdFile::open(file_path)
            .with_context(|| format!("Failed to open file: {}", file_path.display()))?;
        let shared_file = Arc::new(file);

        let mut attempt = 0u32;
        loop {
            // Re-consult the breaker between retry iterations to
            // preserve the "no wire I/O while open" invariant —
            // see the matching comment in `upload`.
            if attempt > 0
                && let Err(BreakerOpen { retry_after }) = self.breaker.check()
            {
                return Err(HttpUploadError {
                    status_code: 503,
                    message: format!(
                        "upload_file: circuit breaker opened during retries; retry after {:.1}s",
                        retry_after.as_secs_f64()
                    ),
                }
                .into());
            }

            let stream = create_read_at_stream(shared_file.clone(), 0, file_size, 0);
            let body = reqwest::Body::wrap_stream(stream);

            let request = self.add_common_headers(
                self.http_client
                    .post(&url)
                    .header("Content-Type", content_type)
                    .header("Content-Length", file_size.to_string())
                    .header("X-Storage-Path", dest_path),
            );

            match request.body(body).send().await {
                Ok(response) if response.status().is_success() => {
                    self.breaker.record(Outcome::Success);
                    return response
                        .json()
                        .await
                        .map_err(|e| anyhow::anyhow!("Failed to parse upload response: {}", e));
                }
                Ok(response) if response.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    self.fire_401_attribution("upload_file");
                    self.breaker.record(Outcome::Failure);
                    return Err(HttpUploadError {
                        status_code: 401,
                        message: format!("{}: HTTP 401 Unauthorized", operation),
                    }
                    .into());
                }
                Ok(response) if response.status() == reqwest::StatusCode::FORBIDDEN => {
                    let body = response.text().await.unwrap_or_default();
                    tracing::warn!("storage upload_file rejected (403): {body}");
                    return Err(HttpUploadError {
                        status_code: 403,
                        message: format!("{operation}: HTTP 403 Forbidden - {body}"),
                    }
                    .into());
                }
                Ok(response) => {
                    let check = ResponseCheck::from_response(response, &operation).await;
                    if check.is_retryable && attempt < self.retry_config.max_retries {
                        check.wait_for_retry(&self.retry_config, attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(HttpUploadError {
                        status_code: check.status_code,
                        message: check.message,
                    }
                    .into());
                }
                Err(e) => {
                    if attempt < self.retry_config.max_retries {
                        wait_for_network_retry(&self.retry_config, &operation, attempt, &e).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(anyhow::anyhow!("{} failed: {}", operation, e));
                }
            }
        }
    }

    /// Uploads content from an async reader to GCS using streaming.
    ///
    /// This is more memory-efficient for large files as it doesn't require
    /// loading the entire content into memory.
    ///
    /// Note: Streaming uploads do not support automatic retries since the stream
    /// can only be consumed once.
    ///
    /// # Arguments
    /// * `bucket` - The GCS bucket name
    /// * `path` - The destination path in the bucket (e.g., "uploads/file.txt")
    /// * `reader` - An async reader providing the content
    /// * `content_type` - MIME type of the content
    ///
    /// # Returns
    /// The upload response containing bucket, path, size, content_type, and generation.
    pub async fn upload_stream<R>(
        &self,
        path: &str,
        reader: R,
        content_type: &str,
    ) -> Result<UploadResponse>
    where
        R: AsyncRead + Send + Sync + 'static,
    {
        let url = format!("{}/storage", self.base_url);

        if let Err(BreakerOpen { retry_after }) = self.breaker.check() {
            // Mirror the wire-401 shape (`anyhow::bail!` below) so
            // callers see the same `Err` payload on both paths.
            anyhow::bail!(
                "Failed to upload to '{}': HTTP 401 Unauthorized (circuit breaker open; retry after {:.1}s)",
                path,
                retry_after.as_secs_f64()
            );
        }

        let stream = ReaderStream::new(reader);
        let body = reqwest::Body::wrap_stream(stream);

        let request = self
            .http_client
            .post(&url)
            .header("Content-Type", content_type)
            .header("X-Storage-Path", path);
        let request = self.add_common_headers(request);
        let response = request
            .body(body)
            .send()
            .await
            .context("Failed to send streaming upload request")?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.fire_401_attribution("upload_stream");
            self.breaker.record(Outcome::Failure);
            anyhow::bail!("Failed to upload to '{}': HTTP 401 Unauthorized", path);
        }
        if status == reqwest::StatusCode::FORBIDDEN {
            let body = response.text().await.unwrap_or_default();
            tracing::warn!("storage upload_stream rejected (403): {body}");
            return Err(HttpUploadError {
                status_code: 403,
                message: format!("Failed to upload to '{path}': HTTP 403 Forbidden - {body}"),
            }
            .into());
        }
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            // Structured so the queue worker can classify terminal 400/404 by
            // status code rather than scraping the message string.
            return Err(HttpUploadError {
                status_code: status.as_u16(),
                message: format!("Failed to upload to '{path}': HTTP {status} - {error_body}"),
            }
            .into());
        }

        self.breaker.record(Outcome::Success);

        let upload_response: UploadResponse = response
            .json()
            .await
            .context("Failed to parse upload response")?;

        Ok(upload_response)
    }

    /// Uploads a large file using multipart upload with parallel chunk uploads.
    ///
    /// This method splits the file into chunks (default 50MB each) and uploads
    /// them in parallel, then triggers server-side compose to create the final object.
    ///
    /// **Direct Upload Mode**: When the server provides pre-signed URLs, parts are
    /// uploaded directly to GCS, bypassing the proxy for data transfer. This is
    /// more efficient as it reduces proxy load and latency.
    ///
    /// **Fallback Mode**: If the server doesn't provide signed URLs (legacy mode),
    /// parts are uploaded through the proxy as before.
    ///
    /// The bucket is determined by the proxy based on the user's ACLs.
    ///
    /// # Arguments
    /// * `path` - The final destination path in the bucket
    /// * `file_path` - Path to the local file to upload
    /// * `content_type` - MIME type of the content (e.g., "application/gzip")
    /// * `options` - Optional configuration for the multipart upload
    ///
    /// # Returns
    /// The multipart complete response containing the final GCS URL and metadata.
    pub async fn upload_multipart(
        &self,
        path: &str,
        file_path: &Path,
        content_type: &str,
        options: Option<MultipartUploadOptions>,
    ) -> Result<MultipartCompleteResponse> {
        let options = options.unwrap_or_default();

        // Get file size to calculate parts
        let metadata = tokio::fs::metadata(file_path)
            .await
            .with_context(|| format!("Failed to get file metadata: {}", file_path.display()))?;
        let file_size = metadata.len();

        if file_size == 0 {
            anyhow::bail!("Cannot upload empty file via multipart");
        }

        // Step 1: Initialize multipart upload session (now with file_size for signed URLs)
        let init_start = std::time::Instant::now();
        let init_response = self.multipart_init(file_size).await?;
        if init_response.upload_id.is_empty() {
            return Ok(MultipartCompleteResponse {
                bucket: String::new(),
                path: path.to_string(),
                gcs_url: String::new(),
                size: 0,
                parts_composed: 0,
                generation: 0,
            });
        }
        let use_direct_upload = !init_response.part_urls.is_empty();
        tracing::debug!(
            "Multipart init completed in {:?}, upload_id={}, direct_upload={}",
            init_start.elapsed(),
            init_response.upload_id,
            use_direct_upload
        );

        let upload_id = init_response.upload_id.clone();
        let part_size = options
            .part_size_bytes
            .unwrap_or(init_response.max_part_size_bytes as usize) as u64;

        // Open file once and share across all parts using read_at (pread)
        let file = StdFile::open(file_path)
            .with_context(|| format!("Failed to open file: {}", file_path.display()))?;
        let shared_file = Arc::new(file);

        // Step 2: Upload parts - either direct to GCS or through proxy
        let uploaded_parts = if use_direct_upload {
            // Direct upload mode: use pre-signed URLs
            tracing::info!(
                "Using direct upload mode with {} pre-signed URLs",
                init_response.part_urls.len()
            );
            self.upload_parts_direct(
                &init_response.part_urls,
                shared_file,
                file_size,
                part_size,
                options.max_concurrent_uploads,
            )
            .await?
        } else {
            // Fallback mode: upload through proxy
            tracing::info!("Using proxy upload mode (no signed URLs)");
            self.upload_parts_via_proxy(
                &upload_id,
                shared_file,
                file_size,
                part_size,
                options.max_concurrent_uploads,
            )
            .await?
        };

        // Step 3: Complete the multipart upload by sending all parts to server
        tracing::debug!(
            "Starting multipart complete with {} parts",
            uploaded_parts.len()
        );
        let complete_start = std::time::Instant::now();
        let result = self
            .multipart_complete(path, &upload_id, uploaded_parts, content_type)
            .await;
        tracing::debug!(
            "Multipart complete finished in {:?}",
            complete_start.elapsed()
        );
        result
    }

    /// Upload parts directly to GCS using pre-signed URLs.
    async fn upload_parts_direct(
        &self,
        part_urls: &[SignedPartUrl],
        shared_file: Arc<StdFile>,
        file_size: u64,
        part_size: u64,
        max_concurrent: usize,
    ) -> Result<Vec<UploadedPartInfo>> {
        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let mut tasks = Vec::with_capacity(part_urls.len());

        for signed_part in part_urls {
            let offset = (signed_part.part_number as u64 - 1) * part_size;
            let length = std::cmp::min(part_size, file_size - offset);

            let permit = semaphore.clone().acquire_owned().await?;
            let signed_url = signed_part.url.clone();
            let part_path = signed_part.path.clone();
            let part_number = signed_part.part_number;
            let file = shared_file.clone();
            let client = self.raw_http_client.clone();
            let retry_config = self.retry_config.clone();

            let task = tokio::spawn(async move {
                let _permit = permit;
                tracing::debug!(
                    "Part {} starting direct upload: offset={}, length={} bytes",
                    part_number,
                    offset,
                    length
                );
                let part_start = std::time::Instant::now();
                let result = upload_part_direct(
                    &client,
                    &signed_url,
                    file,
                    offset,
                    length,
                    part_number,
                    &retry_config,
                )
                .await;
                tracing::debug!(
                    "Part {} direct upload finished in {:?}",
                    part_number,
                    part_start.elapsed()
                );
                result.map(|_| UploadedPartInfo {
                    part_number,
                    path: part_path,
                })
            });

            tasks.push((part_number, task));
        }

        self.collect_upload_results(tasks).await
    }

    /// Upload parts through the proxy (fallback mode).
    async fn upload_parts_via_proxy(
        &self,
        upload_id: &str,
        shared_file: Arc<StdFile>,
        file_size: u64,
        part_size: u64,
        max_concurrent: usize,
    ) -> Result<Vec<UploadedPartInfo>> {
        let num_parts = file_size.div_ceil(part_size);
        tracing::debug!(
            "Uploading {} bytes in {} parts of {} bytes each",
            file_size,
            num_parts,
            part_size
        );

        let semaphore = Arc::new(Semaphore::new(max_concurrent));
        let mut tasks = Vec::with_capacity(num_parts as usize);

        for part_number in 1..=num_parts as u32 {
            let offset = (part_number as u64 - 1) * part_size;
            let length = std::cmp::min(part_size, file_size - offset);

            let permit = semaphore.clone().acquire_owned().await?;
            let upload_id = upload_id.to_string();
            let client = self.http_client.clone();
            let base_url = self.base_url.clone();
            let file = shared_file.clone();
            let retry_config = self.retry_config.clone();

            let task = tokio::spawn(async move {
                let _permit = permit;
                tracing::debug!(
                    "Part {} starting proxy upload: offset={}, length={} bytes",
                    part_number,
                    offset,
                    length
                );
                let part_start = std::time::Instant::now();
                let result = upload_part_streaming(
                    &client,
                    &base_url,
                    &upload_id,
                    part_number,
                    file,
                    offset,
                    length,
                    &retry_config,
                )
                .await;
                tracing::debug!(
                    "Part {} proxy upload finished in {:?}",
                    part_number,
                    part_start.elapsed()
                );
                result.map(|resp| UploadedPartInfo {
                    part_number: resp.part_number,
                    path: resp.path,
                })
            });

            tasks.push((part_number, task));
        }

        self.collect_upload_results(tasks).await
    }

    /// Collect results from upload tasks, handling errors.
    async fn collect_upload_results(
        &self,
        tasks: Vec<(u32, tokio::task::JoinHandle<Result<UploadedPartInfo>>)>,
    ) -> Result<Vec<UploadedPartInfo>> {
        let mut uploaded_parts = Vec::with_capacity(tasks.len());
        let mut upload_errors = Vec::new();

        for (part_num, task) in tasks {
            match task.await {
                Ok(Ok(part_info)) => {
                    tracing::debug!("Part {} uploaded successfully", part_num);
                    uploaded_parts.push(part_info);
                }
                Ok(Err(e)) => {
                    tracing::error!("Part {} upload failed: {}", part_num, e);
                    upload_errors.push((part_num, e));
                }
                Err(e) => {
                    tracing::error!("Part {} task panicked: {}", part_num, e);
                    upload_errors.push((part_num, anyhow::anyhow!("Task panicked: {}", e)));
                }
            }
        }

        if !upload_errors.is_empty() {
            let error_msgs: Vec<String> = upload_errors
                .iter()
                .map(|(part, e)| format!("part {}: {}", part, e))
                .collect();
            anyhow::bail!("Multipart upload failed: {}", error_msgs.join("; "));
        }

        Ok(uploaded_parts)
    }

    /// Initialize a multipart upload session with signed URLs for direct upload.
    /// Includes retry logic for transient failures.
    async fn multipart_init(&self, file_size: u64) -> Result<MultipartInitResponse> {
        let url = format!("{}/storage/multipart/init", self.base_url);
        let operation = "Multipart init";
        let request_body = MultipartInitRequest {
            total_size: file_size,
        };

        let mut attempt = 0u32;
        loop {
            let request = self.add_common_headers(
                self.http_client
                    .post(&url)
                    .header("Content-Type", "application/json"),
            );

            match request.json(&request_body).send().await {
                Ok(response) if response.status().is_success() => {
                    return response
                        .json()
                        .await
                        .context("Failed to parse multipart init response");
                }
                Ok(response) if response.status() == reqwest::StatusCode::FORBIDDEN => {
                    tracing::debug!("storage upload rejected (403), skipping");
                    return Ok(MultipartInitResponse {
                        upload_id: String::new(),
                        bucket: String::new(),
                        max_part_size_bytes: 0,
                        part_urls: vec![],
                        expires_at: None,
                    });
                }
                Ok(response) => {
                    let check = ResponseCheck::from_response(response, operation).await;
                    if check.is_retryable && attempt < self.retry_config.max_retries {
                        check.wait_for_retry(&self.retry_config, attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(HttpUploadError {
                        status_code: check.status_code,
                        message: check.message,
                    }
                    .into());
                }
                Err(e) => {
                    if attempt < self.retry_config.max_retries {
                        wait_for_network_retry(&self.retry_config, operation, attempt, &e).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(e).context("Failed to initialize multipart upload");
                }
            }
        }
    }

    /// Complete a multipart upload by sending parts list to server for compose.
    /// Includes retry logic for transient failures.
    async fn multipart_complete(
        &self,
        path: &str,
        upload_id: &str,
        parts: Vec<UploadedPartInfo>,
        content_type: &str,
    ) -> Result<MultipartCompleteResponse> {
        let url = format!("{}/storage/multipart/{}/complete", self.base_url, upload_id);
        let operation = "Multipart complete";
        let request_body = MultipartCompleteRequest { parts };

        let mut attempt = 0u32;
        loop {
            let request = self.add_common_headers(
                self.http_client
                    .post(&url)
                    .header("X-Storage-Path", path)
                    .header("Content-Type", content_type),
            );

            match request.json(&request_body).send().await {
                Ok(response) if response.status().is_success() => {
                    return response
                        .json()
                        .await
                        .context("Failed to parse multipart complete response");
                }
                Ok(response) => {
                    let check = ResponseCheck::from_response(response, operation).await;
                    if check.is_retryable && attempt < self.retry_config.max_retries {
                        check.wait_for_retry(&self.retry_config, attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(HttpUploadError {
                        status_code: check.status_code,
                        message: check.message,
                    }
                    .into());
                }
                Err(e) => {
                    if attempt < self.retry_config.max_retries {
                        wait_for_network_retry(&self.retry_config, operation, attempt, &e).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(e).context("Failed to complete multipart upload");
                }
            }
        }
    }

    // ====================================================================
    // Signed upload URL
    // ====================================================================

    /// Request a pre-signed GCS PUT URL from the proxy.
    ///
    /// The caller can then upload directly to GCS with a plain HTTP PUT,
    /// completely bypassing the proxy (and its nginx / Cloudflare body-size
    /// limits).  This is the recommended path for payloads that may exceed
    /// 4 MB.
    pub async fn get_signed_upload_url(
        &self,
        path: &str,
        content_type: &str,
    ) -> Result<echo_agent_chat_proxy_types::SignedUploadUrlResponse> {
        let url = format!("{}/storage/signed-upload-url", self.base_url);

        let response = self
            .add_common_headers(
                self.http_client
                    .post(&url)
                    .header("X-Storage-Path", path)
                    .header("Content-Type", content_type),
            )
            .send()
            .await
            .context("Failed to request signed upload URL")?;

        if response.status() == reqwest::StatusCode::FORBIDDEN {
            tracing::debug!("storage upload rejected (403), skipping");
            return Ok(echo_agent_chat_proxy_types::SignedUploadUrlResponse {
                signed_url: String::new(),
                bucket: String::new(),
                path: path.to_string(),
                content_type: content_type.to_string(),
                expires_in_secs: 0,
            });
        }
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Signed upload URL request failed: {status} - {body}");
        }

        response
            .json::<echo_agent_chat_proxy_types::SignedUploadUrlResponse>()
            .await
            .context("Failed to parse signed upload URL response")
    }

    /// Upload bytes directly to GCS using a pre-signed PUT URL.
    ///
    /// The `content_type` **must** match the one baked into the signed URL,
    /// otherwise GCS will reject the request with 403.
    pub async fn upload_via_signed_url(
        &self,
        signed_url: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<()> {
        let response = self
            .raw_http_client
            .put(signed_url)
            .header("Content-Type", content_type)
            .body(data.to_vec())
            .send()
            .await
            .context("Failed to upload via signed URL")?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Signed URL upload failed: {status} - {body}");
        }

        Ok(())
    }

    /// Convenience: request a signed URL and upload bytes in one call.
    ///
    /// Combines [`Self::get_signed_upload_url`] and
    /// [`Self::upload_via_signed_url`] so callers don't need to juggle the
    /// intermediate response.
    pub async fn upload_bytes_signed(
        &self,
        path: &str,
        data: &[u8],
        content_type: &str,
    ) -> Result<echo_agent_chat_proxy_types::SignedUploadUrlResponse> {
        let signed = self.get_signed_upload_url(path, content_type).await?;
        if signed.signed_url.is_empty() {
            return Ok(signed);
        }
        self.upload_via_signed_url(&signed.signed_url, data, &signed.content_type)
            .await?;
        Ok(signed)
    }
}

/// Size of chunks to read from file via read_at (5MB balances syscall overhead vs memory)
const READ_AT_CHUNK_SIZE: usize = 5 << 20;

/// Upload a single part with streaming and retry logic.
///
/// Uses `read_at` (pread) to read from a shared file descriptor without seeking.
/// This allows multiple parts to stream from the same file concurrently.
/// On retry, the stream is recreated from the same offset.
async fn upload_part_streaming(
    client: &reqwest_middleware::ClientWithMiddleware,
    base_url: &str,
    upload_id: &str,
    part_number: u32,
    file: Arc<StdFile>,
    offset: u64,
    length: u64,
    retry_config: &RetryConfig,
) -> Result<MultipartUploadPartResponse> {
    let url = format!(
        "{}/storage/multipart/{}/{}",
        base_url, upload_id, part_number
    );
    let operation = format!("Part {} upload", part_number);

    let mut attempt = 0u32;
    loop {
        let stream_start = std::time::Instant::now();
        let stream = create_read_at_stream(file.clone(), offset, length, part_number);
        let body = reqwest::Body::wrap_stream(stream);
        tracing::debug!(
            "Part {} stream created in {:?}",
            part_number,
            stream_start.elapsed()
        );

        let send_start = std::time::Instant::now();
        tracing::debug!("Part {} sending HTTP request to {}", part_number, url);

        let mut request = client
            .post(&url)
            .header("Content-Type", "application/octet-stream")
            .header("x-echo-agent-client-version", echo_agent_version::VERSION)
            .header("Content-Length", length.to_string());
        for (name, value) in crate::trace_context::trace_context_headers().iter() {
            request = request.header(name.clone(), value.clone());
        }

        match request.body(body).send().await {
            Ok(response) => {
                tracing::debug!(
                    "Part {} HTTP response received in {:?}, status={}",
                    part_number,
                    send_start.elapsed(),
                    response.status()
                );

                if response.status().is_success() {
                    let parse_start = std::time::Instant::now();
                    let part_response: MultipartUploadPartResponse =
                        response.json().await.map_err(|e| {
                            anyhow::anyhow!("Failed to parse part {} response: {}", part_number, e)
                        })?;
                    tracing::debug!(
                        "Part {} response parsed in {:?}",
                        part_number,
                        parse_start.elapsed()
                    );
                    return Ok(part_response);
                }

                let check = ResponseCheck::from_response(response, &operation).await;
                if check.is_retryable && attempt < retry_config.max_retries {
                    check.wait_for_retry(retry_config, attempt).await;
                    attempt += 1;
                    continue;
                }
                return Err(HttpUploadError {
                    status_code: check.status_code,
                    message: check.message,
                }
                .into());
            }
            Err(e) => {
                if attempt < retry_config.max_retries {
                    wait_for_network_retry(retry_config, &operation, attempt, &e).await;
                    attempt += 1;
                    continue;
                }
                return Err(anyhow::anyhow!("{} failed: {}", operation, e));
            }
        }
    }
}

/// Upload a single part directly to GCS using a pre-signed URL.
///
/// This bypasses the proxy entirely - data goes directly to GCS.
/// No authorization headers are needed as the signed URL includes auth.
async fn upload_part_direct(
    client: &Client,
    signed_url: &str,
    file: Arc<StdFile>,
    offset: u64,
    length: u64,
    part_number: u32,
    retry_config: &RetryConfig,
) -> Result<()> {
    let operation = format!("Part {} direct upload", part_number);

    let mut attempt = 0u32;
    loop {
        let stream = create_read_at_stream(file.clone(), offset, length, part_number);
        let body = reqwest::Body::wrap_stream(stream);

        tracing::debug!(
            "Part {} starting network transmission to GCS ({:.1} MB)",
            part_number,
            length as f64 / 1_000_000.0
        );

        let send_start = std::time::Instant::now();
        match client
            .put(signed_url)
            .header("Content-Type", "application/octet-stream")
            .header("Content-Length", length.to_string())
            .body(body)
            .send()
            .await
        {
            Ok(response) => {
                let send_elapsed = send_start.elapsed();
                let status = response.status();
                let speed_mbps = if send_elapsed.as_secs_f64() > 0.0 {
                    (length as f64 / 1_000_000.0) / send_elapsed.as_secs_f64()
                } else {
                    0.0
                };
                tracing::debug!(
                    "Part {} network transmission complete: status={}, total_time={:.2}s, effective_speed={:.2} MB/s",
                    part_number,
                    status,
                    send_elapsed.as_secs_f64(),
                    speed_mbps
                );

                if status.is_success() {
                    return Ok(());
                }

                let check = ResponseCheck::from_response(response, &operation).await;
                if check.is_retryable && attempt < retry_config.max_retries {
                    check.wait_for_retry(retry_config, attempt).await;
                    attempt += 1;
                    continue;
                }
                return Err(HttpUploadError {
                    status_code: check.status_code,
                    message: check.message,
                }
                .into());
            }
            Err(e) => {
                if attempt < retry_config.max_retries {
                    wait_for_network_retry(retry_config, &operation, attempt, &e).await;
                    attempt += 1;
                    continue;
                }
                return Err(anyhow::anyhow!("{} failed: {}", operation, e));
            }
        }
    }
}

/// Creates a stream that reads from a file at the specified offset using `read_at` (pread).
///
/// This is more efficient than seek+read because:
/// 1. No mutex/lock needed on the file position
/// 2. Multiple readers can share the same file descriptor
/// 3. The kernel handles concurrent reads efficiently
fn create_read_at_stream(
    file: Arc<StdFile>,
    start_offset: u64,
    length: u64,
    part_number: u32,
) -> ReceiverStream<Result<Bytes, std::io::Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel(4); // Small buffer for backpressure

    tokio::task::spawn_blocking(move || {
        let mut current_offset = start_offset;
        let end_offset = start_offset + length;
        let mut buf = vec![0u8; READ_AT_CHUNK_SIZE];
        let mut bytes_sent: u64 = 0;
        let stream_start = std::time::Instant::now();
        let mut last_log = std::time::Instant::now();

        while current_offset < end_offset {
            let to_read = std::cmp::min(buf.len() as u64, end_offset - current_offset) as usize;

            let read_start = std::time::Instant::now();
            // Positional read; doesn't move the file cursor.
            #[cfg(unix)]
            let read_result = file.read_at(&mut buf[..to_read], current_offset);
            #[cfg(windows)]
            let read_result = file.seek_read(&mut buf[..to_read], current_offset);
            match read_result {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let read_time = read_start.elapsed();
                    let send_start = std::time::Instant::now();
                    // Send the chunk - if receiver is dropped, stop reading
                    if tx
                        .blocking_send(Ok(Bytes::copy_from_slice(&buf[..n])))
                        .is_err()
                    {
                        break;
                    }
                    let send_time = send_start.elapsed();
                    current_offset += n as u64;
                    bytes_sent += n as u64;

                    // Log progress every 5 seconds or if send took > 100ms
                    if last_log.elapsed().as_secs() >= 5 || send_time.as_millis() > 100 {
                        let elapsed = stream_start.elapsed().as_secs_f64();
                        let speed_mbps = if elapsed > 0.0 {
                            (bytes_sent as f64 / 1_000_000.0) / elapsed
                        } else {
                            0.0
                        };
                        tracing::debug!(
                            "Part {} progress: {:.1}% ({}/{} bytes), speed: {:.2} MB/s, read: {:?}, channel_send: {:?}",
                            part_number,
                            (bytes_sent as f64 / length as f64) * 100.0,
                            bytes_sent,
                            length,
                            speed_mbps,
                            read_time,
                            send_time
                        );
                        last_log = std::time::Instant::now();
                    }
                }
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    break;
                }
            }
        }

        let total_time = stream_start.elapsed();
        let speed_mbps = if total_time.as_secs_f64() > 0.0 {
            (bytes_sent as f64 / 1_000_000.0) / total_time.as_secs_f64()
        } else {
            0.0
        };
        // NOTE: This logs when the producer has finished reading from disk and pushing
        // to the channel. Network transmission may still be in progress as reqwest
        // drains the channel buffer and sends data over the wire.
        // The speed here is throttled by network backpressure (channel_send waits),
        // NOT disk speed.
        tracing::debug!(
            "Part {} file read complete (buffered for network): {} bytes in {:?} ({:.2} MB/s effective, network-throttled)",
            part_number,
            bytes_sent,
            total_time,
            speed_mbps
        );
    });

    ReceiverStream::new(rx)
}

// ============================================================================
// Multipart Upload Types
// ============================================================================

/// Options for multipart upload.
#[derive(Debug, Clone)]
pub struct MultipartUploadOptions {
    /// Size of each part in bytes. Defaults to server's max_part_size_bytes (typically 50MB).
    pub part_size_bytes: Option<usize>,
    /// Maximum number of concurrent part uploads. Defaults to 4.
    pub max_concurrent_uploads: usize,
}

impl Default for MultipartUploadOptions {
    fn default() -> Self {
        Self {
            part_size_bytes: None,
            max_concurrent_uploads: 4,
        }
    }
}

impl MultipartUploadOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_part_size(mut self, size_bytes: usize) -> Self {
        self.part_size_bytes = Some(size_bytes);
        self
    }

    pub fn with_max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent_uploads = max;
        self
    }
}

/// Response from initializing a multipart upload.
/// Bucket is passed via X-Storage-Bucket header.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartInitResponse {
    pub upload_id: String,
    pub bucket: String,
    pub max_part_size_bytes: u64,
    /// Pre-signed PUT URLs for direct upload to GCS (if supported by server)
    #[serde(default)]
    pub part_urls: Vec<SignedPartUrl>,
    /// ISO 8601 timestamp when the signed URLs expire
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// Pre-signed URL information for uploading a single part directly to GCS.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedPartUrl {
    /// Part number (1-indexed)
    pub part_number: u32,
    /// Pre-signed PUT URL for direct upload to GCS
    pub url: String,
    /// GCS path where this part will be stored (for tracking)
    pub path: String,
}

/// Request body for initializing a multipart upload with signed URLs.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MultipartInitRequest {
    /// Total file size in bytes
    total_size: u64,
}

/// Response from uploading a single part.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartUploadPartResponse {
    pub part_number: u32,
    pub path: String,
    pub size: i64,
}

/// Information about an uploaded part (for complete request).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadedPartInfo {
    pub part_number: u32,
    pub path: String,
}

/// Request to complete a multipart upload.
/// Bucket is passed via X-Storage-Bucket header.
/// Path is passed via X-Storage-Path header.
/// Content-Type is passed via Content-Type header.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct MultipartCompleteRequest {
    parts: Vec<UploadedPartInfo>,
}

/// Response from completing a multipart upload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartCompleteResponse {
    pub bucket: String,
    pub path: String,
    pub gcs_url: String,
    pub size: i64,
    pub parts_composed: usize,
    pub generation: i64,
}

#[cfg(test)]
mod download_blob_tests {
    use super::StorageClient;
    use axum::{Router, response::IntoResponse, routing::get};
    use std::net::SocketAddr;
    use tempfile::TempDir;
    use tokio::net::TcpListener;

    async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    fn client_for(addr: SocketAddr) -> StorageClient {
        StorageClient::new(&format!("http://{addr}/v1"), "test-token")
    }

    /// Happy path: proxy returns a signed URL; content server streams back bytes.
    /// Verifies that streaming writes the complete content to disk.
    #[tokio::test]
    async fn download_blob_streams_to_file() {
        const CONTENT: &[u8] = b"chunk-one chunk-two chunk-three";

        let blob_router = Router::new().route(
            "/blob",
            get(|| async { axum::body::Bytes::from_static(CONTENT) }),
        );
        let (blob_addr, _) = start_server(blob_router).await;
        let signed_url = format!("http://{blob_addr}/blob");

        let signed_url_clone = signed_url.clone();
        let proxy_router = Router::new().route(
            "/v1/storage/download",
            get(move || {
                let url = signed_url_clone.clone();
                async move {
                    serde_json::json!({"signed_url": url, "expires_in_secs": 300})
                        .to_string()
                        .into_response()
                }
            }),
        );
        let (proxy_addr, _) = start_server(proxy_router).await;

        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("blob.bin");
        client_for(proxy_addr)
            .download_blob("changes_dedup/v2/blobs/sha256_abc123", &dest)
            .await
            .expect("download_blob must succeed");

        assert_eq!(std::fs::read(&dest).unwrap(), CONTENT);
    }

    /// 403 from the proxy must surface as a clear error; dest file must not be created.
    #[tokio::test]
    async fn download_blob_errors_on_403() {
        let proxy_router = Router::new().route(
            "/v1/storage/download",
            get(|| async { (axum::http::StatusCode::FORBIDDEN, "not internal").into_response() }),
        );
        let (proxy_addr, _) = start_server(proxy_router).await;

        let tmp = TempDir::new().unwrap();
        let dest = tmp.path().join("out.bin");
        let err = client_for(proxy_addr)
            .download_blob("changes_dedup/v2/blobs/sha256_abc123", &dest)
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("403") || err.to_string().contains("not authorized"),
            "error must mention 403 or not authorized, got: {err}"
        );
        assert!(!dest.exists(), "dest must not be created on error");
    }

    /// 400 from the proxy (rejected path) must also surface as an error.
    #[tokio::test]
    async fn download_blob_errors_on_400() {
        let proxy_router = Router::new().route(
            "/v1/storage/download",
            get(|| async { (axum::http::StatusCode::BAD_REQUEST, "bad path").into_response() }),
        );
        let (proxy_addr, _) = start_server(proxy_router).await;

        let err = client_for(proxy_addr)
            .download_blob(
                "changes_dedup/v2/blobs/sha256_abc123",
                &TempDir::new().unwrap().path().join("out"),
            )
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("400"),
            "error must mention 400, got: {err}"
        );
    }
}

#[cfg(test)]
mod batch_check_exists_tests {
    use super::{ExistsResult, StorageClient};
    use axum::{Router, response::IntoResponse, routing::post};
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    fn client_for(addr: SocketAddr) -> StorageClient {
        StorageClient::new(&format!("http://{addr}/v1"), "test-token")
    }

    #[tokio::test]
    async fn batch_returns_existing_paths() {
        let router = Router::new().route(
            "/v1/storage/batch_exists",
            post(|body: axum::Json<serde_json::Value>| async move {
                let paths = body["paths"].as_array().unwrap();
                let exists: Vec<&serde_json::Value> = paths
                    .iter()
                    .filter(|p| p.as_str().unwrap().contains("exists"))
                    .collect();
                let missing: Vec<&serde_json::Value> = paths
                    .iter()
                    .filter(|p| !p.as_str().unwrap().contains("exists"))
                    .collect();
                axum::Json(serde_json::json!({ "exists": exists, "missing": missing }))
                    .into_response()
            }),
        );
        let (addr, _) = start_server(router).await;

        let paths = vec![
            "blobs/exists_a".to_string(),
            "blobs/missing_b".to_string(),
            "blobs/exists_c".to_string(),
        ];
        let result = client_for(addr).batch_check_exists(&paths).await;
        let existing = match result {
            ExistsResult::Found(v) => v,
            other => panic!("should return Found on success, got {:?}", other),
        };

        assert_eq!(existing.len(), 2);
        assert!(existing.contains("blobs/exists_a"));
        assert!(existing.contains("blobs/exists_c"));
        assert!(!existing.contains("blobs/missing_b"));
    }

    #[tokio::test]
    async fn batch_returns_none_on_404() {
        let router = Router::new().route(
            "/v1/storage/batch_exists",
            post(|| async { (axum::http::StatusCode::NOT_FOUND, "").into_response() }),
        );
        let (addr, _) = start_server(router).await;

        let paths = vec!["blobs/a".to_string()];
        let result = client_for(addr).batch_check_exists(&paths).await;
        assert!(
            matches!(result, ExistsResult::NotFound),
            "404 must return NotFound for fallback"
        );
    }

    #[tokio::test]
    async fn batch_returns_probe_failed_on_server_error() {
        let router = Router::new().route(
            "/v1/storage/batch_exists",
            post(|| async {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
            }),
        );
        let (addr, _) = start_server(router).await;

        let paths = vec!["blobs/a".to_string()];
        let result = client_for(addr).batch_check_exists(&paths).await;
        assert!(
            matches!(result, ExistsResult::ProbeFailed),
            "500 must return ProbeFailed so callers don't synthesize a phantom missing set"
        );
    }

    #[tokio::test]
    async fn batch_handles_all_missing() {
        let router = Router::new().route(
            "/v1/storage/batch_exists",
            post(|body: axum::Json<serde_json::Value>| async move {
                let paths = body["paths"].as_array().unwrap().clone();
                axum::Json(serde_json::json!({ "exists": [], "missing": paths })).into_response()
            }),
        );
        let (addr, _) = start_server(router).await;

        let paths = vec!["blobs/a".to_string(), "blobs/b".to_string()];
        let result = client_for(addr).batch_check_exists(&paths).await;
        let existing = match result {
            ExistsResult::Found(v) => v,
            other => panic!("should return Found even when all missing, got {:?}", other),
        };
        assert!(existing.is_empty());
    }

    #[tokio::test]
    async fn batch_handles_empty_input() {
        let router = Router::new().route(
            "/v1/storage/batch_exists",
            post(|| async {
                axum::Json(serde_json::json!({ "exists": [], "missing": [] })).into_response()
            }),
        );
        let (addr, _) = start_server(router).await;

        let paths: &[String] = &[];
        let result = client_for(addr).batch_check_exists(paths).await;
        let existing = match result {
            ExistsResult::Found(v) => v,
            other => panic!("empty input should succeed, got {:?}", other),
        };
        assert!(existing.is_empty());
    }

    #[tokio::test]
    async fn batch_returns_probe_failed_on_malformed_json() {
        let router = Router::new().route(
            "/v1/storage/batch_exists",
            post(|| async { "not valid json {{{" }),
        );
        let (addr, _) = start_server(router).await;

        let paths = vec!["blobs/a".to_string()];
        let result = client_for(addr).batch_check_exists(&paths).await;
        assert!(
            matches!(result, ExistsResult::ProbeFailed),
            "malformed JSON must return ProbeFailed"
        );
    }

    #[tokio::test]
    async fn batch_returns_probe_failed_on_network_error() {
        // Privileged port 1 on loopback isn't listening -> deterministic ECONNREFUSED
        // (no port-reuse race like bind-then-drop would have).
        let client = StorageClient::new("http://127.0.0.1:1/v1", "test-token");
        let paths = vec!["blobs/a".to_string()];
        let result = client.batch_check_exists(&paths).await;
        assert!(
            matches!(result, ExistsResult::ProbeFailed),
            "connection refused must return ProbeFailed, got {:?}",
            result
        );
    }

    /// `&[String]` and `&[&str]` produce a byte-identical request body.
    #[tokio::test]
    async fn batch_check_exists_accepts_string_and_str_slices_with_identical_body() {
        use std::sync::{Arc as StdArc, Mutex};

        let captured: StdArc<Mutex<Vec<String>>> = StdArc::new(Mutex::new(Vec::new()));
        let captured_h = captured.clone();
        let router = Router::new().route(
            "/v1/storage/batch_exists",
            post(move |body: axum::body::Bytes| {
                let captured_h = captured_h.clone();
                async move {
                    captured_h
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(body.to_vec()).unwrap());
                    axum::Json(serde_json::json!({"exists": []}))
                }
            }),
        );
        let (addr, _) = start_server(router).await;
        let client = client_for(addr);

        // &[String]
        let owned: Vec<String> = vec!["a".to_string(), "b".to_string()];
        let r1 = client.batch_check_exists(&owned).await;
        assert!(matches!(
            r1,
            ExistsResult::NotFound | ExistsResult::Found(_)
        ));

        // &[&str]
        let borrowed: Vec<&str> = vec!["a", "b"];
        let r2 = client.batch_check_exists(&borrowed).await;
        assert!(matches!(
            r2,
            ExistsResult::NotFound | ExistsResult::Found(_)
        ));

        let bodies = captured.lock().unwrap();
        assert_eq!(bodies.len(), 2, "two requests captured");
        assert_eq!(bodies[0], r#"{"paths":["a","b"]}"#, "&[String] wire format");
        assert_eq!(
            bodies[0], bodies[1],
            "&[String] and &[&str] must produce byte-identical bodies"
        );
    }
}

#[cfg(test)]
mod check_exists_tests {
    //! Mirrors `batch_check_exists_tests` so the singular `check_exists` has
    //! the same coverage of Found / NotFound / Unauthorized / ProbeFailed
    //! discriminants after canonicalisation.
    use super::{ExistsResult, StorageClient};
    use axum::{Router, response::IntoResponse, routing::get};
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    fn client_for(addr: SocketAddr) -> StorageClient {
        StorageClient::new(&format!("http://{addr}/v1"), "test-token")
    }

    #[tokio::test]
    async fn check_exists_returns_found_on_2xx() {
        let router = Router::new().route(
            "/v1/storage/exists",
            get(|| async {
                axum::Json(serde_json::json!({
                    "bucket": "b",
                    "path": "p",
                    "size": 42,
                    "content_type": "application/octet-stream",
                    "generation": 1
                }))
                .into_response()
            }),
        );
        let (addr, _) = start_server(router).await;
        let result = client_for(addr).check_exists("p").await;
        match result {
            ExistsResult::Found(resp) => {
                assert_eq!(resp.bucket, "b");
                assert_eq!(resp.path, "p");
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn check_exists_returns_not_found_on_404() {
        let router = Router::new().route(
            "/v1/storage/exists",
            get(|| async { (axum::http::StatusCode::NOT_FOUND, "").into_response() }),
        );
        let (addr, _) = start_server(router).await;
        let result = client_for(addr).check_exists("p").await;
        assert!(matches!(result, ExistsResult::NotFound));
    }

    #[tokio::test]
    async fn check_exists_returns_unauthorized_on_403() {
        let router = Router::new().route(
            "/v1/storage/exists",
            get(|| async { (axum::http::StatusCode::FORBIDDEN, "").into_response() }),
        );
        let (addr, _) = start_server(router).await;
        let result = client_for(addr).check_exists("p").await;
        assert!(
            matches!(result, ExistsResult::Unauthorized),
            "403 must map to Unauthorized (symmetric with batch_check_exists)"
        );
    }

    #[tokio::test]
    async fn check_exists_returns_unauthorized_on_401() {
        let router = Router::new().route(
            "/v1/storage/exists",
            get(|| async { (axum::http::StatusCode::UNAUTHORIZED, "").into_response() }),
        );
        let (addr, _) = start_server(router).await;
        let result = client_for(addr).check_exists("p").await;
        assert!(matches!(result, ExistsResult::Unauthorized));
    }

    #[tokio::test]
    async fn check_exists_returns_probe_failed_on_500() {
        let router = Router::new().route(
            "/v1/storage/exists",
            get(|| async {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
            }),
        );
        let (addr, _) = start_server(router).await;
        let result = client_for(addr).check_exists("p").await;
        assert!(
            matches!(result, ExistsResult::ProbeFailed),
            "500 must return ProbeFailed (not collapse to NotFound)"
        );
    }

    #[tokio::test]
    async fn check_exists_returns_probe_failed_on_malformed_json() {
        let router =
            Router::new().route("/v1/storage/exists", get(|| async { "not valid json {{{" }));
        let (addr, _) = start_server(router).await;
        let result = client_for(addr).check_exists("p").await;
        assert!(
            matches!(result, ExistsResult::ProbeFailed),
            "malformed JSON must return ProbeFailed"
        );
    }

    #[tokio::test]
    async fn check_exists_returns_probe_failed_on_network_error() {
        let client = StorageClient::new("http://127.0.0.1:1/v1", "test-token");
        let result = client.check_exists("p").await;
        assert!(
            matches!(result, ExistsResult::ProbeFailed),
            "connection refused must return ProbeFailed, got {result:?}"
        );
    }
}

#[cfg(test)]
mod batch_upload_tests {
    use super::StorageClient;
    use axum::{Router, response::IntoResponse, routing::post};
    use echo_agent_chat_proxy_types::BatchUploadStatus;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::TcpListener;

    async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    fn client_for(addr: SocketAddr) -> StorageClient {
        StorageClient::new(&format!("http://{addr}/v1"), "test-token")
    }

    fn test_files() -> Vec<(String, Vec<u8>, String)> {
        vec![(
            "data/hello.txt".to_string(),
            b"hello world".to_vec(),
            "text/plain".to_string(),
        )]
    }

    #[tokio::test]
    async fn batch_upload_returns_none_on_404() {
        let router = Router::new().route(
            "/v1/storage/batch_upload",
            post(|| async { (axum::http::StatusCode::NOT_FOUND, "").into_response() }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr).batch_upload(test_files()).await;
        assert!(result.is_none(), "404 must return None for backward compat");
    }

    #[tokio::test]
    async fn batch_upload_returns_results_on_success() {
        let router = Router::new().route(
            "/v1/storage/batch_upload",
            post(|| async {
                axum::Json(serde_json::json!({
                    "results": [{
                        "path": "data/hello.txt",
                        "status": "ok",
                        "bucket": "test-bucket",
                        "size": 11,
                        "generation": 1,
                    }]
                }))
                .into_response()
            }),
        );
        let (addr, _) = start_server(router).await;

        let results = client_for(addr)
            .batch_upload(test_files())
            .await
            .expect("should return Some on success");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "data/hello.txt");
        assert_eq!(results[0].status, BatchUploadStatus::Ok);
        assert_eq!(results[0].size, Some(11));
    }

    #[tokio::test]
    async fn batch_upload_returns_none_on_server_error() {
        let router = Router::new().route(
            "/v1/storage/batch_upload",
            post(|| async {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "boom").into_response()
            }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr).batch_upload(test_files()).await;
        assert!(result.is_none(), "500 must return None");
    }

    #[tokio::test]
    async fn batch_upload_returns_none_on_malformed_json() {
        let router = Router::new().route(
            "/v1/storage/batch_upload",
            post(|| async { "not valid json {{{" }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr).batch_upload(test_files()).await;
        assert!(result.is_none(), "malformed JSON must return None");
    }

    #[tokio::test]
    async fn batch_upload_defaults_empty_content_type() {
        let router = Router::new().route(
            "/v1/storage/batch_upload",
            post(|| async {
                axum::Json(serde_json::json!({
                    "results": [{ "path": "blob", "status": "ok", "bucket": "b", "size": 4, "generation": 1 }]
                }))
                .into_response()
            }),
        );
        let (addr, _) = start_server(router).await;

        let files = vec![("blob".to_string(), b"data".to_vec(), String::new())];
        let results = client_for(addr)
            .batch_upload(files)
            .await
            .expect("empty content_type should default, not panic");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].status, BatchUploadStatus::Ok);
    }

    #[tokio::test]
    async fn batch_upload_parses_mixed_results() {
        let router = Router::new().route(
            "/v1/storage/batch_upload",
            post(|| async {
                axum::Json(serde_json::json!({
                    "results": [
                        { "path": "a.txt", "status": "ok", "bucket": "b", "size": 3, "generation": 1 },
                        { "path": "b.bin", "status": "error", "error": "gcs timeout" },
                        { "path": "c.bin", "status": "skipped", "bucket": "b", "size": 99, "generation": 2 },
                    ]
                }))
                .into_response()
            }),
        );
        let (addr, _) = start_server(router).await;

        let results = client_for(addr)
            .batch_upload(test_files())
            .await
            .expect("should return Some");

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].status, BatchUploadStatus::Ok);
        assert_eq!(results[0].size, Some(3));
        assert_eq!(results[1].status, BatchUploadStatus::Error);
        assert_eq!(results[1].error.as_deref(), Some("gcs timeout"));
        assert_eq!(results[2].status, BatchUploadStatus::Skipped);
        assert_eq!(results[2].size, Some(99));
    }

    /// Integration test: verify the client's multipart form is correctly parsed
    /// by a server that extracts field names (like the proxy does). This catches
    /// the bug where paths with "/" in field names were not received by multer.
    #[tokio::test]
    async fn batch_upload_paths_with_slashes_parsed_correctly() {
        use axum::extract::Multipart;

        async fn handler(mut multipart: Multipart) -> axum::response::Response {
            let mut results = Vec::new();
            while let Ok(Some(field)) = multipart.next_field().await {
                let name = field.name().unwrap_or("").to_string();
                let file_name = field.file_name().unwrap_or("").to_string();
                // Use name if available, fall back to file_name (matches proxy fix)
                let path = if name.is_empty() { file_name } else { name };
                if path.is_empty() {
                    return (
                        axum::http::StatusCode::BAD_REQUEST,
                        "Multipart field missing name",
                    )
                        .into_response();
                }
                let data = field.bytes().await.unwrap_or_default();
                results.push(serde_json::json!({
                    "path": path,
                    "status": "ok",
                    "bucket": "test-bucket",
                    "size": data.len(),
                    "generation": 1,
                }));
            }
            axum::Json(serde_json::json!({ "results": results })).into_response()
        }

        let router = Router::new().route("/v1/storage/batch_upload", post(handler));
        let (addr, _) = start_server(router).await;

        let files = vec![
            (
                "changes_dedup/v2/blobs/sha1_abc123".to_string(),
                b"blob1".to_vec(),
                "application/octet-stream".to_string(),
            ),
            (
                "changes_dedup/v2/patches/sha256_def456".to_string(),
                b"file2".to_vec(),
                "application/octet-stream".to_string(),
            ),
            (
                "simple_name".to_string(),
                b"file3".to_vec(),
                "text/plain".to_string(),
            ),
        ];

        let results = client_for(addr)
            .batch_upload(files)
            .await
            .expect("should parse paths with slashes");

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].path, "changes_dedup/v2/blobs/sha1_abc123");
        assert_eq!(results[1].path, "changes_dedup/v2/patches/sha256_def456");
        assert_eq!(results[2].path, "simple_name");
        assert!(results.iter().all(|r| r.status == BatchUploadStatus::Ok));
    }

    #[tokio::test]
    async fn batch_upload_retries_on_503_then_succeeds() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempt = Arc::new(AtomicU32::new(0));
        let attempt_clone = attempt.clone();

        let handler = move || {
            let attempt = attempt_clone.clone();
            async move {
                let n = attempt.fetch_add(1, Ordering::Relaxed);
                if n == 0 {
                    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "busy").into_response()
                } else {
                    axum::Json(serde_json::json!({
                        "results": [{
                            "path": "data/hello.txt",
                            "status": "ok",
                            "bucket": "b",
                            "size": 11,
                            "generation": 1
                        }]
                    }))
                    .into_response()
                }
            }
        };

        let router = Router::new().route("/v1/storage/batch_upload", post(handler));
        let (addr, _) = start_server(router).await;

        let client = client_for(addr).with_retry_config(
            super::RetryConfig::new()
                .with_initial_delay(std::time::Duration::from_millis(1))
                .with_max_retries(3),
        );
        let result = client.batch_upload(test_files()).await;
        assert!(result.is_some(), "should succeed after retry");
        assert_eq!(attempt.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn batch_upload_gives_up_after_max_retries() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempt = Arc::new(AtomicU32::new(0));
        let attempt_clone = attempt.clone();

        let handler = move || {
            let attempt = attempt_clone.clone();
            async move {
                attempt.fetch_add(1, Ordering::Relaxed);
                (axum::http::StatusCode::BAD_GATEWAY, "always fail").into_response()
            }
        };

        let router = Router::new().route("/v1/storage/batch_upload", post(handler));
        let (addr, _) = start_server(router).await;

        let client = client_for(addr).with_retry_config(
            super::RetryConfig::new()
                .with_initial_delay(std::time::Duration::from_millis(1))
                .with_max_retries(2),
        );
        let result = client.batch_upload(test_files()).await;
        assert!(result.is_none(), "should give up after max retries");
        // 1 initial + 2 retries = 3 total attempts
        assert_eq!(attempt.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn batch_upload_does_not_retry_on_400() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempt = Arc::new(AtomicU32::new(0));
        let attempt_clone = attempt.clone();

        let handler = move || {
            let attempt = attempt_clone.clone();
            async move {
                attempt.fetch_add(1, Ordering::Relaxed);
                (axum::http::StatusCode::BAD_REQUEST, "bad input").into_response()
            }
        };

        let router = Router::new().route("/v1/storage/batch_upload", post(handler));
        let (addr, _) = start_server(router).await;

        let client = client_for(addr).with_retry_config(
            super::RetryConfig::new()
                .with_initial_delay(std::time::Duration::from_millis(1))
                .with_max_retries(3),
        );
        let result = client.batch_upload(test_files()).await;
        assert!(result.is_none(), "400 should not retry");
        assert_eq!(attempt.load(Ordering::Relaxed), 1);
    }
}

#[cfg(test)]
mod batch_upload_json_tests {
    use super::StorageClient;
    use axum::{Router, response::IntoResponse, routing::post};
    use echo_agent_chat_proxy_types::BatchUploadStatus;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio_util::bytes;

    async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    fn client_for(addr: SocketAddr) -> StorageClient {
        StorageClient::new(&format!("http://{addr}/v1"), "test-token")
    }

    fn test_files() -> Vec<(String, Vec<u8>, String)> {
        vec![(
            "data/hello.txt".to_string(),
            b"hello world".to_vec(),
            "text/plain".to_string(),
        )]
    }

    /// Parse a batch_upload_json request (zstd-compressed JSON with base64 file data).
    fn parse_batch_json_body(
        headers: &axum::http::HeaderMap,
        body: &[u8],
    ) -> echo_agent_chat_proxy_types::BatchUploadRequest {
        let json_bytes = if headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case("zstd"))
        {
            zstd::decode_all(std::io::Cursor::new(body)).expect("mock: invalid zstd body")
        } else {
            body.to_vec()
        };
        serde_json::from_slice(&json_bytes).expect("mock: invalid JSON body")
    }

    #[tokio::test]
    async fn batch_upload_json_returns_results_on_success() {
        let router = Router::new().route(
            "/v1/storage/batch_upload_json",
            post(
                |headers: axum::http::HeaderMap, body: bytes::Bytes| async move {
                    assert_eq!(
                        headers.get("content-encoding").unwrap().to_str().unwrap(),
                        "zstd"
                    );
                    assert_eq!(
                        headers.get("content-type").unwrap().to_str().unwrap(),
                        "application/json"
                    );
                    let req = parse_batch_json_body(&headers, &body);
                    assert_eq!(req.files.len(), 1);
                    assert_eq!(req.files[0].path, "data/hello.txt");

                    use base64::Engine;
                    let decoded = base64::engine::general_purpose::STANDARD
                        .decode(&req.files[0].data)
                        .unwrap();
                    assert_eq!(decoded, b"hello world");

                    axum::Json(serde_json::json!({
                        "results": [{
                            "path": "data/hello.txt",
                            "status": "ok",
                            "bucket": "test-bucket",
                            "size": 11,
                            "generation": 1,
                        }]
                    }))
                    .into_response()
                },
            ),
        );
        let (addr, _) = start_server(router).await;

        let results = client_for(addr)
            .batch_upload_json(test_files())
            .await
            .expect("should return Some on success");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "data/hello.txt");
        assert_eq!(results[0].status, BatchUploadStatus::Ok);
        assert_eq!(results[0].size, Some(11));
    }

    #[tokio::test]
    async fn batch_upload_json_returns_none_on_404() {
        let router = Router::new().route(
            "/v1/storage/batch_upload_json",
            post(|| async { (axum::http::StatusCode::NOT_FOUND, "").into_response() }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr).batch_upload_json(test_files()).await;
        assert!(result.is_none(), "404 must return None");
    }

    #[tokio::test]
    async fn batch_upload_json_returns_none_on_403() {
        let router = Router::new().route(
            "/v1/storage/batch_upload_json",
            post(|| async { (axum::http::StatusCode::FORBIDDEN, "forbidden").into_response() }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr).batch_upload_json(test_files()).await;
        assert!(result.is_none(), "403 must return None");
    }

    #[tokio::test]
    async fn batch_upload_json_retries_on_503_then_succeeds() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempt = Arc::new(AtomicU32::new(0));
        let attempt_clone = attempt.clone();

        let handler = move || {
            let attempt = attempt_clone.clone();
            async move {
                let n = attempt.fetch_add(1, Ordering::Relaxed);
                if n == 0 {
                    (axum::http::StatusCode::SERVICE_UNAVAILABLE, "busy").into_response()
                } else {
                    axum::Json(serde_json::json!({
                        "results": [{
                            "path": "data/hello.txt",
                            "status": "ok",
                            "bucket": "b",
                            "size": 11,
                            "generation": 1
                        }]
                    }))
                    .into_response()
                }
            }
        };

        let router = Router::new().route("/v1/storage/batch_upload_json", post(handler));
        let (addr, _) = start_server(router).await;

        let client = client_for(addr).with_retry_config(
            super::RetryConfig::new()
                .with_initial_delay(std::time::Duration::from_millis(1))
                .with_max_retries(3),
        );
        let result = client.batch_upload_json(test_files()).await;
        assert!(result.is_some(), "should succeed after retry");
        assert_eq!(attempt.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn batch_upload_json_short_circuits_on_empty_input() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let hits = Arc::new(AtomicU32::new(0));
        let hits_clone = hits.clone();
        let router = Router::new().route(
            "/v1/storage/batch_upload_json",
            post(move || {
                let hits = hits_clone.clone();
                async move {
                    hits.fetch_add(1, Ordering::Relaxed);
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR
                }
            }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr).batch_upload_json(Vec::new()).await;
        let results = result.expect("empty input should return Some(empty)");
        assert!(results.is_empty(), "results must be empty for empty input");
        assert_eq!(
            hits.load(Ordering::Relaxed),
            0,
            "no HTTP request must be made for empty input"
        );
    }

    #[tokio::test]
    async fn batch_upload_json_does_not_retry_on_400() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let attempt = Arc::new(AtomicU32::new(0));
        let attempt_clone = attempt.clone();

        let handler = move || {
            let attempt = attempt_clone.clone();
            async move {
                attempt.fetch_add(1, Ordering::Relaxed);
                (axum::http::StatusCode::BAD_REQUEST, "bad input").into_response()
            }
        };

        let router = Router::new().route("/v1/storage/batch_upload_json", post(handler));
        let (addr, _) = start_server(router).await;

        let client = client_for(addr).with_retry_config(
            super::RetryConfig::new()
                .with_initial_delay(std::time::Duration::from_millis(1))
                .with_max_retries(3),
        );
        let result = client.batch_upload_json(test_files()).await;
        assert!(result.is_none(), "400 should not retry");
        assert_eq!(attempt.load(Ordering::Relaxed), 1);
    }
}

#[cfg(test)]
mod forbidden_tests {
    use super::*;
    use axum::{Router, response::IntoResponse, routing::post};
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    fn client_for(addr: SocketAddr) -> StorageClient {
        StorageClient::new(&format!("http://{addr}/v1"), "test-token")
    }

    fn forbidden_handler() -> impl IntoResponse {
        (axum::http::StatusCode::FORBIDDEN, "ZDR team")
    }

    #[tokio::test]
    async fn upload_returns_err_on_403() {
        let router = Router::new().route("/v1/storage", post(|| async { forbidden_handler() }));
        let (addr, _) = start_server(router).await;

        let err = client_for(addr)
            .upload("path/file.txt", b"data", "text/plain")
            .await
            .unwrap_err();
        let upload_err = err
            .downcast_ref::<HttpUploadError>()
            .expect("must be HttpUploadError");
        assert_eq!(upload_err.status_code, 403);
    }

    #[tokio::test]
    async fn upload_file_returns_err_on_403() {
        let router = Router::new().route("/v1/storage", post(|| async { forbidden_handler() }));
        let (addr, _) = start_server(router).await;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"content").unwrap();

        let err = client_for(addr)
            .upload_file("dest/file.bin", tmp.path(), "application/octet-stream")
            .await
            .unwrap_err();
        let upload_err = err
            .downcast_ref::<HttpUploadError>()
            .expect("must be HttpUploadError");
        assert_eq!(upload_err.status_code, 403);
    }

    #[tokio::test]
    async fn upload_stream_returns_err_on_403() {
        let router = Router::new().route("/v1/storage", post(|| async { forbidden_handler() }));
        let (addr, _) = start_server(router).await;

        let reader = std::io::Cursor::new(b"stream data");
        let err = client_for(addr)
            .upload_stream("path/stream.bin", reader, "application/octet-stream")
            .await
            .unwrap_err();
        let upload_err = err
            .downcast_ref::<HttpUploadError>()
            .expect("must be HttpUploadError");
        assert_eq!(upload_err.status_code, 403);
    }

    #[tokio::test]
    async fn upload_multipart_returns_ok_on_403() {
        let router = Router::new().route(
            "/v1/storage/multipart/init",
            post(|| async { forbidden_handler() }),
        );
        let (addr, _) = start_server(router).await;

        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), b"some content").unwrap();

        let result = client_for(addr)
            .upload_multipart(
                "path/large.bin",
                tmp.path(),
                "application/octet-stream",
                None,
            )
            .await;
        let resp = result.expect("403 on multipart init must return Ok");
        assert!(resp.bucket.is_empty());
        assert_eq!(resp.parts_composed, 0);
    }

    #[tokio::test]
    async fn get_signed_upload_url_returns_ok_on_403() {
        let router = Router::new().route(
            "/v1/storage/signed-upload-url",
            post(|| async { forbidden_handler() }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr)
            .get_signed_upload_url("path/file.bin", "application/octet-stream")
            .await;
        let resp = result.expect("403 must return Ok");
        assert!(resp.signed_url.is_empty());
    }

    #[tokio::test]
    async fn upload_bytes_signed_returns_ok_on_403() {
        let router = Router::new().route(
            "/v1/storage/signed-upload-url",
            post(|| async { forbidden_handler() }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr)
            .upload_bytes_signed("path/file.bin", b"data", "text/plain")
            .await;
        let resp = result.expect("403 must return Ok without attempting signed upload");
        assert!(resp.signed_url.is_empty());
    }

    #[tokio::test]
    async fn batch_check_exists_returns_unauthorized_on_403() {
        let router = Router::new().route(
            "/v1/storage/batch_exists",
            post(|| async { forbidden_handler() }),
        );
        let (addr, _) = start_server(router).await;

        let result = client_for(addr)
            .batch_check_exists(&["a".to_string()])
            .await;
        assert!(
            matches!(result, ExistsResult::Unauthorized),
            "403 must return Unauthorized (symmetric with S3)"
        );
    }

    #[tokio::test]
    async fn batch_upload_returns_none_on_403() {
        let router = Router::new().route(
            "/v1/storage/batch_upload",
            post(|| async { forbidden_handler() }),
        );
        let (addr, _) = start_server(router).await;

        let files = vec![(
            "data/hello.txt".to_string(),
            b"hello world".to_vec(),
            "text/plain".to_string(),
        )];
        let result = client_for(addr).batch_upload(files).await;
        assert!(result.is_none(), "403 must return None");
    }
}

#[cfg(test)]
#[path = "storage_client_breaker_tests.rs"]
mod breaker_tests;
