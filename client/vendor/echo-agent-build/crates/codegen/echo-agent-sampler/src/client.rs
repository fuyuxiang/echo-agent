//! HTTP client for the EchoAgent sampling APIs.
//!
//! Owns the `reqwest::Client`, default request headers, and per-method
//! defaults. Talks to three backend shapes:
//!
//! * Chat Completions (`/chat/completions`)
//! * Responses API (`/responses`)
//! * Anthropic Messages API (`/messages`)
//!
//! All trace-upload and URL-based header injection is intentionally
//! *not* here. The session is responsible for putting any per-request
//! headers (proxy auth, OTel context, etc.)
//! into [`SamplerConfig::extra_headers`] before constructing the client.

use eventsource_stream::{EventStreamError, Eventsource};
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use indexmap::IndexMap;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, USER_AGENT,
};
use serde::Serialize;

use echo_agent_sampling_types::error::{
    parse_error_code, try_parse_stream_error, user_facing_api_error_message,
};
use echo_agent_sampling_types::{
    ChatCompletionChunk, ChatCompletionRequest, ChatCompletionResponse, ConversationRequest,
    ConversationResponse, CreateResponseWrapper, DOOM_LOOP_CHECK_HEADER, MessagesRequestWrapper,
    ResponseModelMetadata, Result, SamplingError, SentCredential, build_messages_request,
    is_check_event, messages, rs,
};

use crate::config::{AuthScheme, OriginClientInfo, SamplerConfig};
use crate::events::{SamplingErrorInfo, SamplingErrorKind};
use crate::stream::chat_completions::ChatChoiceState;
use echo_agent_auth::bearer_suffix;

// Re-export ApiBackend from the shared types crate for downstream callers.
pub use echo_agent_sampling_types::ApiBackend;

/// Process-level fallback for the `x-echo-agent-client-identifier` header.
const DEFAULT_CLIENT_IDENTIFIER: &str = "echo-agent-shell";

/// Product identifier baked into User-Agent strings.
const AGENT_PRODUCT: &str = "echo-agent-shell";
const ANTHROPIC_DEFAULT_MAX_TOKENS: u32 = 128_000;
const ENFORCE_SECURE_PROVIDER_URLS_ENV: &str = "ECHO_AGENT_ENFORCE_SECURE_PROVIDER_URLS";
const ALLOW_INSECURE_LOOPBACK_HTTP_ENV: &str = "ECHO_AGENT_ALLOW_INSECURE_LOOPBACK_HTTP";

/// URLs explicitly approved by the desktop's personal-connection settings.
/// The allowlist is replaced after each config write, so approval for one
/// provider cannot silently authorize a different HTTP endpoint.
static APPROVED_INSECURE_HTTP_URLS: std::sync::OnceLock<
    std::sync::RwLock<std::collections::HashSet<String>>,
> = std::sync::OnceLock::new();

fn approved_insecure_http_urls() -> &'static std::sync::RwLock<std::collections::HashSet<String>> {
    APPROVED_INSECURE_HTTP_URLS
        .get_or_init(|| std::sync::RwLock::new(std::collections::HashSet::new()))
}

fn normalized_http_base_url(raw: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(raw.trim()).ok()?;
    if parsed.scheme() != "http"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return None;
    }
    Some(parsed.to_string().trim_end_matches('/').to_owned())
}

/// Replace the desktop-approved HTTP endpoints. Only native settings code
/// should call this after validating each personal provider's explicit opt-in.
pub fn replace_approved_insecure_http_urls(urls: impl IntoIterator<Item = String>) {
    let approved = urls
        .into_iter()
        .filter_map(|url| normalized_http_base_url(&url))
        .collect();
    *approved_insecure_http_urls()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = approved;
}

fn is_approved_insecure_http_url(raw: &str) -> bool {
    normalized_http_base_url(raw).is_some_and(|url| {
        approved_insecure_http_urls()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains(&url)
    })
}

fn env_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"))
}

fn validate_sampling_base_url_with_policy(
    raw: &str,
    enforce: bool,
    allow_insecure_loopback: bool,
) -> Result<()> {
    validate_sampling_base_url_with_approval(
        raw,
        enforce,
        allow_insecure_loopback,
        is_approved_insecure_http_url(raw),
    )
}

fn validate_sampling_base_url_with_approval(
    raw: &str,
    enforce: bool,
    allow_insecure_loopback: bool,
    approved_remote_http: bool,
) -> Result<()> {
    if !enforce {
        return Ok(());
    }
    let parsed = reqwest::Url::parse(raw)
        .map_err(|_| SamplingError::InvalidConfiguration("invalid model provider base URL"))?;
    if parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(SamplingError::InvalidConfiguration(
            "model provider base URL contains forbidden components",
        ));
    }
    match parsed.scheme() {
        "https" => Ok(()),
        // This exact built-in endpoint has no credential and is also allowed
        // by the desktop provider validator. Keep both policies in sync.
        "http" if raw.trim().trim_end_matches('/') == "http://123.56.188.16:8088/v1" => Ok(()),
        "http" if approved_remote_http => Ok(()),
        "http"
            if allow_insecure_loopback
                && parsed.host_str().is_some_and(|host| {
                    matches!(host, "localhost" | "127.0.0.1")
                        || host.trim_matches(['[', ']']) == "::1"
                }) =>
        {
            Ok(())
        }
        "http" => Err(SamplingError::InvalidConfiguration(
            "model provider HTTP requires explicit personal-connection approval; loopback development also supports ECHO_AGENT_ALLOW_INSECURE_LOOPBACK_HTTP=1",
        )),
        _ => Err(SamplingError::InvalidConfiguration(
            "model provider base URL must use HTTPS",
        )),
    }
}

fn validate_sampling_base_url(raw: &str) -> Result<()> {
    validate_sampling_base_url_with_policy(
        raw,
        env_enabled(ENFORCE_SECURE_PROVIDER_URLS_ENV),
        env_enabled(ALLOW_INSECURE_LOOPBACK_HTTP_ENV),
    )
}

/// Per-request `x-echo-agent-*` headers. Optional fields are skipped when empty/`None`.
struct EchoAgentRequestHeaders<'a> {
    conv_id: &'a str,
    req_id: &'a str,
    model_id: &'a str,
    session_id: &'a str,
    turn_idx: Option<&'a str>,
    agent_id: &'a str,
    deployment_id: Option<&'a str>,
    user_id: Option<&'a str>,
}

impl EchoAgentRequestHeaders<'_> {
    fn apply(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut b = builder
            .header("x-echo-agent-conv-id", self.conv_id)
            .header("x-echo-agent-req-id", self.req_id)
            .header("x-echo-agent-model-override", self.model_id)
            .header("x-echo-agent-session-id", self.session_id)
            .header("x-echo-agent-agent-id", self.agent_id);
        if let Some(idx) = self.turn_idx {
            b = b.header("x-echo-agent-turn-idx", idx);
        }
        if let Some(id) = self.deployment_id.filter(|s| !s.is_empty()) {
            b = b.header("x-echo-agent-deployment-id", id);
        }
        if let Some(id) = self.user_id.filter(|s| !s.is_empty()) {
            b = b.header("x-echo-agent-user-id", id);
        }
        b
    }
}

/// Parse the `Retry-After` response header as delta-seconds.
/// Our inference backends only emit integer seconds (never HTTP-date),
/// so we only handle that form. HTTP-dates silently return `None` and
/// the caller falls back to exponential backoff.
/// Capped at 120s to prevent absurdly long sleeps from a misbehaving upstream.
/// Deserialize a Responses API SSE event, with a fallback for EchoAgent-specific
/// tool types (e.g., `x_search`) that `async_openai` can't parse.
///
/// The API echoes the request's `tools` array in `ResponseCompleted` and
/// `ResponseCreated` events. If we sent `{"type": "x_search"}`, the response
/// includes it, and `rs::Tool` deserialization fails. On failure, we strip
/// unrecognized tools from the raw JSON and retry.
///
/// On `response.completed` / `response.incomplete`, this also rewrites
/// `response.usage.total_tokens` in place to the live context length
/// (`context_details.input_tokens + context_details.output_tokens`)
/// when the API emits the EchoAgent-specific `context_details` field.
/// Async-openai's typed `ResponseUsage` doesn't model `context_details`,
/// so we peek the raw JSON for it. The cumulative `input_tokens` /
/// `output_tokens` / `cached_tokens` continue to flow from the typed
/// `ResponseUsage` unchanged so billing telemetry stays correct. When
/// the API doesn't emit `context_details` (older deployments) `total_tokens`
/// passes through unchanged.
fn deserialize_response_event(data: &str) -> Result<rs::ResponseStreamEvent> {
    let mut event = match serde_json::from_str::<rs::ResponseStreamEvent>(data) {
        Ok(event) => event,
        Err(first_err) => {
            // Try sanitizing: parse as Value, strip unknown tools, retry.
            if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data) {
                // Strip tools that async_openai's rs::Tool can't deserialize
                // (e.g., EchoAgent-specific "x_search"). Instead of maintaining a
                // hardcoded allowlist, try deserializing each tool entry —
                // if it fails, drop it.
                if let Some(tools) = value
                    .pointer_mut("/response/tools")
                    .and_then(|v| v.as_array_mut())
                {
                    tools.retain(|t| serde_json::from_value::<rs::Tool>(t.clone()).is_ok());
                }
                if let Ok(mut event) = serde_json::from_value::<rs::ResponseStreamEvent>(value) {
                    apply_terminal_event_overrides(&mut event, data);
                    return Ok(event);
                }
            }
            tracing::error!(
                error = %first_err,
                payload_bytes = data.len(),
                "Failed to deserialize ResponseStreamEvent from stream"
            );
            return Err(SamplingError::Serialization(first_err));
        }
    };
    apply_terminal_event_overrides(&mut event, data);
    Ok(event)
}

/// On terminal Responses API events (`response.completed` /
/// `response.incomplete`), rewrite `response.usage.total_tokens` to the
/// live context length when the wire includes
/// `response.usage.context_details.{input_tokens, output_tokens}`.
///
/// `total_tokens` drives the CLI's `/context` bar, the auto-compact
/// threshold, and `meta.totalTokens` on persisted sessions. Under
/// server-side multi-turn loops (e.g. `web_search`, `x_search`) the
/// wire's cumulative total inflates as the loop runs; `context_details`
/// reports the final turn's prompt + output tokens — the real live
/// context the model is sitting in. Billing fields
/// (`input_tokens`, `output_tokens`, `input_tokens_details.cached_tokens`,
/// `output_tokens_details.reasoning_tokens`) stay on the cumulative
/// wire values so telemetry is unaffected.
///
/// No-op when:
/// - the event is not terminal,
/// - `response.usage` is `None`,
/// - `context_details` is absent (older backends / non-loop responses),
/// - or either of `context_details.{input_tokens, output_tokens}` is
///   missing — we don't guess the missing half.
fn apply_terminal_event_overrides(event: &mut rs::ResponseStreamEvent, data: &str) {
    let response = match event {
        rs::ResponseStreamEvent::ResponseCompleted(e) => &mut e.response,
        rs::ResponseStreamEvent::ResponseIncomplete(e) => &mut e.response,
        _ => return,
    };
    // Re-parse for fields async_openai's types omit (context total, cost ticks).
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    // Stash cost ticks in metadata for stream_responses.
    if let Some(ticks) = echo_agent_sampling_types::reported_cost_ticks(
        value
            .pointer("/response/usage/cost_in_usd_ticks")
            .and_then(|v| v.as_i64()),
    ) {
        response
            .metadata
            .get_or_insert_with(Default::default)
            .insert(COST_USD_TICKS_METADATA_KEY.to_owned(), ticks.to_string());
    }
    let Some(usage) = response.usage.as_mut() else {
        return;
    };
    let Some(total) = extract_context_total(&value) else {
        return;
    };
    usage.total_tokens = total;
}

/// Metadata key for cost ticks past typed Response events.
pub(crate) const COST_USD_TICKS_METADATA_KEY: &str = "echoagent.cost_usd_ticks";

/// Read `response.usage.context_details.{input_tokens, output_tokens}`
/// from the parsed terminal-event JSON and return their sum. Returns `None`
/// if either field is missing or out of `u32` range.
fn extract_context_total(value: &serde_json::Value) -> Option<u32> {
    let cd = value.pointer("/response/usage/context_details")?;
    let i = u32::try_from(cd.get("input_tokens")?.as_u64()?).ok()?;
    let o = u32::try_from(cd.get("output_tokens")?.as_u64()?).ok()?;
    Some(i.saturating_add(o))
}

/// Record `success=false` + `error` on the active inference span when a stream
/// request fails before any response (transport/connect/TLS errors). Without
/// this the `#[instrument]` span closes with both fields Empty, so an outage
/// shows zero `success=false` and error-rate alerts never fire.
fn record_stream_request_failure(err: &reqwest::Error) {
    let span = tracing::Span::current();
    span.record("success", false);
    span.record("error", err.to_string().as_str());
}

/// Splice the raw-JSON hosted-tool entries for `web_search` and `x_search` into a serialized
/// Responses request body's `tools` array. `x_search` has no `rs::Tool` variant at all, and
/// `web_search` has one whose typed filters cannot carry `excluded_domains`, so both travel as raw
/// JSON and neither may also be emitted as a typed `rs::Tool` (the API rejects the duplicate).
/// Shared by the streaming (`create_response_stream`) and non-streaming (`create_response`) paths
/// so neither can silently drop these tools.
fn splice_extra_tool_entries(
    request_body: &mut serde_json::Value,
    entries: Vec<serde_json::Value>,
) {
    if entries.is_empty() {
        return;
    }
    if let Some(tools) = request_body.get_mut("tools").and_then(|v| v.as_array_mut()) {
        tools.extend(entries);
    } else {
        request_body["tools"] = serde_json::Value::Array(entries);
    }
}

fn extract_retry_after(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| s.min(120))
}

fn extract_should_retry(headers: &reqwest::header::HeaderMap) -> Option<bool> {
    headers
        .get("x-should-retry")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            if s.eq_ignore_ascii_case("true") {
                Some(true)
            } else if s.eq_ignore_ascii_case("false") {
                Some(false)
            } else {
                None // unknown value — treat as absent
            }
        })
}

fn extract_model_metadata(headers: &reqwest::header::HeaderMap) -> Option<ResponseModelMetadata> {
    let context_window = headers
        .get("x-echo-agent-context-window")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());

    let max_completion_tokens = headers
        .get("x-echo-agent-max-completion-tokens")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok());

    let models_etag = headers
        .get("x-models-etag")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    if context_window.is_some() || max_completion_tokens.is_some() || models_etag.is_some() {
        Some(ResponseModelMetadata {
            context_window,
            max_completion_tokens,
            models_etag,
        })
    } else {
        None
    }
}

/// Wrapper for streaming chat completion requests that adds `stream` and
/// `stream_options` fields without modifying the original `ChatCompletionRequest`.
///
/// Uses `#[serde(flatten)]` to inline all fields from the inner request,
/// allowing single-pass serialization instead of the previous two-pass
/// approach (serialize to `Value`, mutate, serialize to bytes).
#[derive(Serialize)]
struct StreamingChatRequest<'a> {
    #[serde(flatten)]
    inner: &'a ChatCompletionRequest,
    stream: bool,
    stream_options: StreamOptions,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

fn append_response_includes(body: &mut serde_json::Value, extra_includes: &[String]) {
    if extra_includes.is_empty() {
        return;
    }
    let Some(body) = body.as_object_mut() else {
        return;
    };
    let include = body.entry("include").or_insert(serde_json::Value::Null);
    if include.is_null() {
        *include = serde_json::Value::Array(Vec::new());
    }
    let Some(include) = include.as_array_mut() else {
        return;
    };
    for value in extra_includes {
        if !include
            .iter()
            .any(|existing| existing.as_str() == Some(value.as_str()))
        {
            include.push(serde_json::Value::String(value.clone()));
        }
    }
}

/// Resolve `env_http_headers` (`header -> env var`) into `headers` via `getenv`, skipping unset/blank/invalid entries and trimming values.
fn apply_env_http_headers(
    env_http_headers: &IndexMap<String, String>,
    getenv: impl Fn(&str) -> Option<String>,
    headers: &mut HeaderMap,
) {
    for (key, env_var) in env_http_headers {
        let Some(value) = getenv(env_var) else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let (Ok(name), Ok(header_value)) = (
            HeaderName::try_from(key.as_str()),
            HeaderValue::from_str(value),
        ) else {
            tracing::warn!(
                header = %key,
                env_var = %env_var,
                "skipping env_http_header with an invalid header name or value"
            );
            continue;
        };
        headers.insert(name, header_value);
    }
}

/// HTTP client for sampling. Cheap to clone; carries an `Arc`-backed
/// `reqwest::Client` and the default headers/request-defaults computed from a
/// [`SamplerConfig`] at construction time.
#[derive(Clone)]
pub struct SamplingClient {
    http: reqwest::Client,
    default_headers: HeaderMap,
    base_url: String,
    defaults: ClientDefaults,
    /// Optional 401-attribution hook. The shell wires this to emit a
    /// structured event at every UNAUTHORIZED arm so 401s can be
    /// bucketed by stale-snapshot vs. live-token-rejected. `None` for
    /// sampler-only callers and tests.
    attribution_callback: Option<crate::attribution::SharedAttributionCallback>,
    /// Per-request bearer override. See `SamplerConfig::bearer_resolver`.
    bearer_resolver: Option<crate::config::SharedBearerResolver>,
    /// Per-request header injection (OTel traceparent).
    header_injector: Option<crate::config::SharedHeaderInjector>,
    /// Endpoint URL builder, resolved once from `base_url` + `query_params`.
    endpoint: EndpointTemplate,
}

impl std::fmt::Debug for SamplingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SamplingClient")
            .field("base_url", &self.base_url)
            .field("defaults", &self.defaults)
            .field(
                "has_attribution_callback",
                &self.attribution_callback.is_some(),
            )
            .field("has_bearer_resolver", &self.bearer_resolver.is_some())
            .finish()
    }
}

#[derive(Clone, Debug, Default)]
struct ClientDefaults {
    model: String,
    max_completion_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    api_backend: ApiBackend,
    auth_scheme: AuthScheme,
    stream_tool_calls: bool,
    extra_response_includes: Vec<String>,
    doom_loop_recovery: Option<echo_agent_sampling_types::DoomLoopRecoveryPolicy>,
}

/// Endpoint URL builder, resolved once at client construction so each request
/// only appends its path.
#[derive(Clone, Debug)]
enum EndpointTemplate {
    /// No query params and no query on the base URL (or an unparseable base):
    /// append the path to the base verbatim.
    Plain(String),
    /// Query params configured: `{prefix}/{path}{suffix}`. `suffix` starts with
    /// `?` and folds any base-URL params, with a configured key winning over the
    /// same key in `base_url` (percent-encoded, no duplicates).
    WithQuery { prefix: String, suffix: String },
}

impl EndpointTemplate {
    fn new(base_url: &str, query_params: &IndexMap<String, String>) -> Self {
        let base = base_url.trim_end_matches('/').to_string();
        // The fast path is safe only when there is nothing to fold: no configured
        // params and no query already on the base (which would otherwise land
        // before the appended path).
        if query_params.is_empty() && !base.contains('?') {
            return Self::Plain(base);
        }
        let mut url = match reqwest::Url::parse(&base) {
            Ok(url) => url,
            Err(error) => {
                tracing::warn!(
                    url = %base,
                    %error,
                    "failed to parse base URL for endpoint; sending without folded query"
                );
                return Self::Plain(base);
            }
        };
        let overridden: std::collections::HashSet<&str> =
            query_params.keys().map(String::as_str).collect();
        let kept: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(k, _)| !overridden.contains(k.as_ref()))
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        let prefix = {
            let mut prefix_url = url.clone();
            prefix_url.set_query(None);
            prefix_url.as_str().trim_end_matches('/').to_string()
        };
        {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            for (key, value) in &kept {
                pairs.append_pair(key, value);
            }
            for (key, value) in query_params {
                pairs.append_pair(key, value);
            }
        }
        let suffix = url.query().map(|q| format!("?{q}")).unwrap_or_default();
        Self::WithQuery { prefix, suffix }
    }

    fn url_for_path(&self, path: &str) -> String {
        let path = path.trim_start_matches('/');
        match self {
            Self::Plain(base) => format!("{base}/{path}"),
            Self::WithQuery { prefix, suffix } => format!("{prefix}/{path}{suffix}"),
        }
    }
}

// =============================================================================
// User-Agent helpers
// =============================================================================

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlatformInfo {
    os: String,
    arch: String,
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

fn agent_version() -> String {
    echo_agent_version::VERSION.to_string()
}

/// Render a User-Agent string for the given origin client.
///
/// Mirrors the shell's `user_agent_string_for` but uses sampler-local
/// constants. The session typically owns the canonical User-Agent
/// rendering for process-wide HTTP clients; this helper is for
/// per-session sampling clients that want to override it.
pub fn user_agent_string_for(origin: &OriginClientInfo) -> String {
    let agent_version = agent_version();
    let platform = PlatformInfo::current();

    if origin.product == AGENT_PRODUCT && origin.version.as_deref() == Some(agent_version.as_str())
    {
        return format!(
            "{}/{} ({}; {})",
            AGENT_PRODUCT, agent_version, platform.os, platform.arch
        );
    }

    match origin.version.as_deref() {
        Some(origin_version) => format!(
            "{}/{} {}/{} ({}; {})",
            origin.product,
            origin_version,
            AGENT_PRODUCT,
            agent_version,
            platform.os,
            platform.arch
        ),
        None => format!(
            "{} {}/{} ({}; {})",
            origin.product, AGENT_PRODUCT, agent_version, platform.os, platform.arch
        ),
    }
}

/// A request builder coupled to the credential state it was built with, so
/// a 401 arm cannot classify from anything but the build-time capture. The
/// wire default (`SentCredential::Unknown`, which charges the retry budget)
/// stays the fail-closed one; only an explicit `sent_bearer: None` — a send
/// the builder provably stamped no credential onto — reaches the uncharged
/// lane via [`auth_rejected`].
struct SentRequest {
    builder: reqwest::RequestBuilder,
    /// Tail fragment of the credential in the built headers (`None` = no
    /// credential header at all).
    sent_bearer: Option<String>,
}

/// The one way a 401 becomes a `SamplingError::Auth` with a wire-derived
/// credential classification: from the fragment its [`SentRequest`] captured.
fn auth_rejected(message: String, sent_bearer: Option<&str>) -> SamplingError {
    SamplingError::Auth {
        message,
        credential: SentCredential::from_sent_fragment(sent_bearer),
    }
}

// =============================================================================
// SamplingClient
// =============================================================================

impl SamplingClient {
    /// Construct a sampling client from a [`SamplerConfig`].
    ///
    /// Grabs the process-wide shared `reqwest::Client` (HTTP/2 by
    /// default, HTTP/1.1 when `config.force_http1` is set) and
    /// pre-computes the default request headers. This does not perform
    /// any network I/O.
    pub fn new(config: SamplerConfig) -> Result<Self> {
        validate_sampling_base_url(&config.base_url)?;
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref api_key) = config.api_key {
            match config.auth_scheme {
                AuthScheme::XApiKey => {
                    let header_value = HeaderValue::from_str(api_key).map_err(|_| {
                        tracing::debug!(
                            "Invalid api_key: cannot be converted to a valid HTTP header"
                        );
                        SamplingError::auth_unknown(
                            "Invalid api_key: cannot be converted to a valid HTTP header",
                        )
                    })?;
                    headers.insert(HeaderName::from_static("x-api-key"), header_value);
                }
                AuthScheme::Bearer => {
                    let bearer = format!("Bearer {}", api_key);
                    let header_value = HeaderValue::from_str(&bearer).map_err(|_| {
                        tracing::debug!(
                            "Invalid api_key: cannot be converted to a valid HTTP Authorization header"
                        );
                        SamplingError::auth_unknown(
                            "Invalid api_key: cannot be converted to a valid HTTP Authorization header",
                        )
                    })?;
                    headers.insert(AUTHORIZATION, header_value);
                }
            }
        }

        // Apply all extra headers verbatim. This is the single
        // injection point for proxy-auth headers and any other URL- or
        // environment-specific headers the session decides to set.
        for (key, value) in &config.extra_headers {
            let header_name = HeaderName::try_from(key.as_str())
                .map_err(|_| SamplingError::InvalidConfiguration("Invalid extra header name"))?;
            let header_value = HeaderValue::from_str(value)
                .map_err(|_| SamplingError::InvalidConfiguration("Invalid extra header value"))?;
            headers.insert(header_name, header_value);
        }

        // Resolve here, not into `extra_headers`, so an env-sourced secret stays
        // out of persisted state.
        apply_env_http_headers(
            &config.env_http_headers,
            |var| std::env::var(var).ok(),
            &mut headers,
        );

        // Add x-echo-agent-client-version header for version gating at the proxy.
        if let Some(client_version) = config.client_version.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(client_version)
        {
            headers.insert(
                HeaderName::from_static("x-echo-agent-client-version"),
                header_value,
            );
        }

        if let Some(deployment_id) = config.deployment_id.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(deployment_id)
        {
            headers.insert(
                HeaderName::from_static("x-echo-agent-deployment-id"),
                header_value,
            );
        }

        if let Some(user_id) = config.user_id.as_ref()
            && let Ok(header_value) = HeaderValue::from_str(user_id)
        {
            headers.insert(
                HeaderName::from_static("x-echo-agent-user-id"),
                header_value,
            );
        }

        {
            let client_id = config
                .client_identifier
                .clone()
                .unwrap_or_else(|| DEFAULT_CLIENT_IDENTIFIER.to_string());
            if let Ok(header_value) = HeaderValue::from_str(&client_id) {
                headers.insert(
                    HeaderName::from_static("x-echo-agent-client-identifier"),
                    header_value,
                );
            }
        }

        // Always set User-Agent: per-session origin if available, else fallback.
        {
            let ua_string = match config.origin_client.as_ref() {
                Some(origin) => user_agent_string_for(origin),
                None => user_agent_string_for(&OriginClientInfo {
                    product: AGENT_PRODUCT.to_string(),
                    version: Some(agent_version()),
                }),
            };
            if let Ok(v) = HeaderValue::from_str(&ua_string) {
                headers.insert(USER_AGENT, v);
            }
        }

        let http = if config.force_http1 {
            tracing::info!("Using HTTP/1.1 for sampling client (force_http1=true)");
            crate::shared_http::client_http1().map_err(SamplingError::Http)?
        } else {
            crate::shared_http::client().map_err(SamplingError::Http)?
        };

        tracing::info!(
            target: crate::sampling_log::TARGET,
            event = "client_new",
            model = %config.model,
            api_backend = ?config.api_backend,
            auth_scheme = ?config.auth_scheme,
            // "unset" (not "none"): `ReasoningEffort::None` is a real wire value;
            // logging the absent Option as "none" looked like we were sending it.
            reasoning_effort = config.reasoning_effort.map_or("unset", |e| e.as_str()),
            has_api_key = config.api_key.is_some(),
            has_bearer_resolver = config.bearer_resolver.is_some(),
            has_authorization_header = headers.get(AUTHORIZATION).is_some(),
            has_x_api_key_header = headers.get(HeaderName::from_static("x-api-key")).is_some(),
        );

        let defaults = ClientDefaults {
            model: config.model,
            max_completion_tokens: config.max_completion_tokens,
            temperature: config.temperature,
            top_p: config.top_p,
            api_backend: config.api_backend,
            auth_scheme: config.auth_scheme,
            stream_tool_calls: config.stream_tool_calls,
            extra_response_includes: config.extra_response_includes,
            doom_loop_recovery: config.doom_loop_recovery,
        };

        let endpoint = EndpointTemplate::new(&config.base_url, &config.query_params);

        Ok(Self {
            http,
            default_headers: headers,
            base_url: config.base_url,
            defaults,
            attribution_callback: config.attribution_callback,
            bearer_resolver: config.bearer_resolver,
            header_injector: config.header_injector,
            endpoint,
        })
    }

    /// The configured API backend for this client.
    pub fn api_backend(&self) -> ApiBackend {
        self.defaults.api_backend.clone()
    }

    /// POST with default headers, returning the builder coupled to the tail
    /// fragment of the credential actually placed in its headers (`None` =
    /// no credential) — captured at build time because a record-time
    /// re-read races with the recovery a 401 triggers.
    ///
    /// A wired bearer_resolver is the sole auth source: a missing live
    /// bearer strips default Authorization / x-api-key so a hard-expired
    /// seed key cannot ride on the wire.
    fn post(&self, url: impl reqwest::IntoUrl) -> SentRequest {
        let mut headers = self.default_headers.clone();
        if let Some(resolver) = &self.bearer_resolver {
            headers.remove(AUTHORIZATION);
            headers.remove(HeaderName::from_static("x-api-key"));
            if let Some(fresh) = resolver.current_bearer() {
                match self.defaults.auth_scheme {
                    AuthScheme::XApiKey => {
                        if let Ok(v) = HeaderValue::from_str(&fresh) {
                            headers.insert(HeaderName::from_static("x-api-key"), v);
                        }
                    }
                    AuthScheme::Bearer => {
                        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {fresh}")) {
                            headers.insert(AUTHORIZATION, v);
                        }
                    }
                }
            }
        }
        tracing::info!(
            target: crate::sampling_log::TARGET,
            event = "client_post",
            model = %self.defaults.model,
            api_backend = ?self.defaults.api_backend,
            auth_scheme = ?self.defaults.auth_scheme,
            has_bearer_resolver = self.bearer_resolver.is_some(),
            has_authorization_header = headers.get(AUTHORIZATION).is_some(),
            has_x_api_key_header = headers.get(HeaderName::from_static("x-api-key")).is_some(),
        );
        let sent_bearer = Self::sent_fragment_from_headers(&headers, &self.defaults.auth_scheme);
        if let Some(injector) = &self.header_injector {
            injector.inject(&mut headers);
        }
        SentRequest {
            builder: self.http.post(url).headers(headers),
            sent_bearer,
        }
    }

    /// Tail fragment of the credential in `headers` — `x-api-key`
    /// (Messages-API scheme) or `Authorization` — per
    /// [`crate::attribution::BEARER_SUFFIX_LEN`].
    fn sent_fragment_from_headers(headers: &HeaderMap, scheme: &AuthScheme) -> Option<String> {
        let raw = match scheme {
            AuthScheme::XApiKey => headers
                .get(HeaderName::from_static("x-api-key"))
                .and_then(|v| v.to_str().ok()),
            AuthScheme::Bearer => headers
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer ")),
        };
        raw.map(|s| bearer_suffix(s).to_string())
    }

    /// Best-effort *build-time* view of what the next request would carry.
    /// Kept for attribution-focused tests; production request logs record only
    /// whether credentials are present and never retain this fragment.
    #[cfg(test)]
    fn current_sent_bearer_suffix(&self) -> Option<String> {
        if self.bearer_resolver.is_some() {
            return self
                .bearer_resolver
                .as_ref()
                .and_then(|r| r.current_bearer())
                .map(|s| bearer_suffix(&s).to_string());
        }
        Self::sent_fragment_from_headers(&self.default_headers, &self.defaults.auth_scheme)
    }

    fn has_current_auth(&self) -> bool {
        if let Some(resolver) = self.bearer_resolver.as_ref() {
            return resolver.current_bearer().is_some();
        }
        match self.defaults.auth_scheme {
            AuthScheme::XApiKey => self
                .default_headers
                .contains_key(HeaderName::from_static("x-api-key")),
            AuthScheme::Bearer => self
                .default_headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("Bearer ")),
        }
    }

    /// Invoke the optional 401 attribution callback for one logical
    /// 401 response. Each of the six UNAUTHORIZED arms in this file
    /// calls this helper immediately before returning
    /// `SamplingError::Auth(...)`. Emit happens at the lowest layer
    /// that saw the status, so higher layers that react to a 401 must
    /// not emit a duplicate event.
    ///
    /// `sent_suffix` is the fragment [`Self::post`] captured for the
    /// rejected request (already tail-truncated; the full bearer never
    /// crosses this boundary).
    fn record_401_attribution(
        &self,
        consumer: crate::attribution::SamplingConsumer,
        sent_suffix: Option<&str>,
    ) {
        if let Some(cb) = self.attribution_callback.as_ref() {
            cb.record_401(consumer, sent_suffix);
        }
    }

    pub fn auth_info(&self) -> crate::sampling_log::AuthInfo {
        let has_auth = self.has_current_auth();
        let auth_type = match (&self.defaults.auth_scheme, has_auth) {
            (AuthScheme::XApiKey, true) => "x-api-key",
            (AuthScheme::Bearer, true) => "bearer",
            (_, false) => "none",
        };
        crate::sampling_log::AuthInfo {
            auth_type,
            has_auth,
        }
    }

    /// Log header names only. Custom gateways can put secrets in arbitrary
    /// extension headers whose names do not match a reliable deny-list.
    fn log_request_headers(request: &reqwest::Request, endpoint_name: &str) {
        for (name, value) in request.headers().iter() {
            tracing::debug!(
                header_name = %name,
                header_value_bytes = value.as_bytes().len(),
                "Request header ({})",
                endpoint_name
            );
        }
    }

    fn endpoint(&self, path: &str) -> String {
        self.endpoint.url_for_path(path)
    }

    fn apply_defaults(&self, mut request: ChatCompletionRequest) -> Result<ChatCompletionRequest> {
        if request.model.is_none() {
            request.model = Some(self.defaults.model.clone());
        }

        if request.max_tokens.is_none() {
            request.max_tokens = self.defaults.max_completion_tokens;
        }

        if request.temperature.is_none() {
            request.temperature = self.defaults.temperature;
        }

        if request.top_p.is_none() {
            request.top_p = self.defaults.top_p;
        }

        Ok(request)
    }

    /// `sent_bearer` is the fragment [`Self::post`] captured for the
    /// request that produced `response` (401 attribution).
    async fn handle_response(
        &self,
        response: reqwest::Response,
        sent_bearer: Option<&str>,
    ) -> Result<ChatCompletionResponse> {
        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = response.bytes().await?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ChatCompletions,
                    sent_bearer,
                );
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401): {server_message}"),
                    sent_bearer,
                ));
            }
            let message = user_facing_api_error_message(status, bytes.as_ref());
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
                error_code: parse_error_code(bytes.as_ref()),
            });
        }

        let completion = serde_json::from_slice::<ChatCompletionResponse>(&bytes).map_err(|e| {
            tracing::error!(
                error = %e,
                payload_bytes = bytes.len(),
                "Failed to deserialize ChatCompletionResponse"
            );
            SamplingError::Serialization(e)
        })?;
        Ok(completion)
    }

    // =========================================================================
    // Chat Completions API
    // =========================================================================

    pub async fn chat_completion(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse> {
        let payload = self.apply_defaults(request)?;
        let x_echo_agent_conv_id = &payload.x_echo_agent_conv_id.clone().unwrap_or_default();
        let x_echo_agent_req_id = &payload.x_echo_agent_req_id.clone().unwrap_or_default();
        let model_id = payload.model.clone().unwrap_or_default();

        tracing::debug!(model_id = %model_id, "Sending chat completion request");

        let echo_agent_headers = EchoAgentRequestHeaders {
            conv_id: x_echo_agent_conv_id,
            req_id: x_echo_agent_req_id,
            model_id: &model_id,
            session_id: payload
                .x_echo_agent_session_id
                .as_deref()
                .unwrap_or_default(),
            turn_idx: payload.x_echo_agent_turn_idx.as_deref(),
            agent_id: payload.x_echo_agent_agent_id.as_deref().unwrap_or_default(),
            deployment_id: payload.x_echo_agent_deployment_id.as_deref(),
            user_id: payload.x_echo_agent_user_id.as_deref(),
        };
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post(self.endpoint("chat/completions"));
        let http_request = echo_agent_headers.apply(builder).json(&payload);

        let response = http_request.send().await.map_err(|e| {
            // Log at debug level; errors are surfaced to the caller.
            tracing::debug!("HTTP request failed: {}", e);
            e
        })?;

        self.handle_response(response, sent_bearer.as_deref()).await
    }

    /// Start a streaming chat completion request. Returns a stream of typed chunks.
    #[tracing::instrument(
        name = "http.chat_completion_stream",
        skip_all,
        fields(
            model_id = request.model.as_deref().unwrap_or(""),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub async fn chat_completion_stream(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<(
        BoxStream<'static, Result<ChatCompletionChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        let payload = self.apply_defaults(request)?;
        let x_echo_agent_conv_id = &payload.x_echo_agent_conv_id.clone().unwrap_or_default();
        let x_echo_agent_req_id = &payload.x_echo_agent_req_id.clone().unwrap_or_default();
        let model_id = payload.model.clone().unwrap_or_default();

        // Wrap the request with streaming fields and serialize once.
        // Previously this path serialized twice: first to serde_json::Value
        // (to inject `stream` and `stream_options`), then to HTTP body bytes.
        let streaming_request = StreamingChatRequest {
            inner: &payload,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };

        let echo_agent_headers = EchoAgentRequestHeaders {
            conv_id: x_echo_agent_conv_id,
            req_id: x_echo_agent_req_id,
            model_id: &model_id,
            session_id: payload
                .x_echo_agent_session_id
                .as_deref()
                .unwrap_or_default(),
            turn_idx: payload.x_echo_agent_turn_idx.as_deref(),
            agent_id: payload.x_echo_agent_agent_id.as_deref().unwrap_or_default(),
            deployment_id: payload.x_echo_agent_deployment_id.as_deref(),
            user_id: payload.x_echo_agent_user_id.as_deref(),
        };
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post(self.endpoint("chat/completions"));
        let http_request = echo_agent_headers
            .apply(builder)
            .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
            .json(&streaming_request);

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;

        tracing::debug!(
            url = %built_request.url(),
            method = %built_request.method(),
            "Sending chat/completions request"
        );
        Self::log_request_headers(&built_request, "chat/completions");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ChatCompletionsStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("chat/completions");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                payload_bytes = bytes.len(),
                model_id = %model_id,
                "chat/completions API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
                error_code: parse_error_code(bytes.as_ref()),
            });
        }

        // Strip UTF-8 BOM if present: eventsource-stream 0.2.3 incorrectly slices BOM at byte 1 instead of 3.
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        let chunks = async_stream::try_stream! {
            futures_util::pin_mut!(event_stream);
            let mut choices = ChatChoiceState::default();
            while let Some(event_res) = event_stream.next().await {
                let event = match event_res {
                    Ok(event) => event,
                    // The choice already finished. A transport failure while
                    // reading the optional usage trailer cannot undo that.
                    Err(EventStreamError::Transport(_)) if choices.finish_reason.is_some() => return,
                    Err(EventStreamError::Transport(error)) => Err(SamplingError::ChatStreamInterrupted {
                        detail: error.to_string(),
                        retryable: true,
                    })?,
                    Err(_) => Err(SamplingError::ChatStreamInterrupted {
                        detail: "Malformed Chat Completions SSE frame".into(),
                        retryable: false,
                    })?,
                };
                let data = &event.data;
                if data == "[DONE]" {
                    // Accept [DONE] alone from providers that omit all
                    // per-choice finish reasons.
                    if choices.saw_finish_reason && choices.finish_reason.is_none() {
                        Err(SamplingError::ChatStreamInterrupted {
                            detail: "Chat Completions ended only an alternate choice".into(),
                            retryable: false,
                        })?;
                    }
                    return;
                }

                tracing::info!(
                    target: crate::sampling_log::TARGET,
                    event = "sse_chunk",
                    backend = "chat_completions",
                    payload_bytes = data.len(),
                );

                if let Some(stream_error) = try_parse_stream_error(data) {
                    Err(stream_error)?;
                }
                let chunk = serde_json::from_str::<ChatCompletionChunk>(data).map_err(|e| {
                    tracing::error!(
                        error = %e,
                        payload_bytes = data.len(),
                        "Failed to deserialize ChatCompletionChunk from stream"
                    );
                    SamplingError::Serialization(e)
                })?;

                for choice in &chunk.choices {
                    choices.observe(choice)?;
                }
                yield chunk;
            }

            // End-of-body is not a terminal marker. Retain compatibility with
            // providers that omit [DONE] after an explicit finish_reason.
            if choices.finish_reason.is_none() {
                Err(SamplingError::ChatStreamInterrupted {
                    detail: "Chat Completions stream closed before the selected choice finished".into(),
                    retryable: true,
                })?;
            }
        }
        .boxed();

        Ok((chunks, model_metadata))
    }

    // =========================================================================
    // Responses API
    // =========================================================================

    /// Apply default configuration to a Responses API request.
    fn apply_response_defaults(&self, request: &mut CreateResponseWrapper) -> Result<()> {
        // Apply model default if not specified
        if request.inner.model.is_none() {
            request.inner.model = Some(self.defaults.model.clone());
        }

        // Apply temperature default if not specified
        if request.inner.temperature.is_none() {
            request.inner.temperature = self.defaults.temperature;
        }

        // Apply top_p default if not specified
        if request.inner.top_p.is_none() {
            request.inner.top_p = self.defaults.top_p;
        }

        // Apply max_output_tokens default if not specified
        if request.inner.max_output_tokens.is_none() {
            request.inner.max_output_tokens = self.defaults.max_completion_tokens;
        }

        // Set store to false if not specified (default is true, but that breaks ZDR compliance)
        if request.inner.store.is_none() {
            request.inner.store = Some(false);
        }

        // Include encrypted reasoning content if not specified
        let includes = request.inner.include.get_or_insert_with(Vec::new);
        if !includes.contains(&rs::IncludeEnum::ReasoningEncryptedContent) {
            includes.push(rs::IncludeEnum::ReasoningEncryptedContent);
        }

        Ok(())
    }

    /// Create a response using the Responses API (non-streaming).
    ///
    /// This uses the Responses API format which provides a simpler interface
    /// for multi-turn conversations and tool calling.
    pub async fn create_response(
        &self,
        mut request: CreateResponseWrapper,
    ) -> Result<rs::Response> {
        self.apply_response_defaults(&mut request)?;

        let x_echo_agent_conv_id = request.x_echo_agent_conv_id.as_deref().unwrap_or_default();
        let x_echo_agent_req_id = request.x_echo_agent_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone().unwrap_or_default();

        // The trace field is process-local: it is consumed by upstream
        // session code (which may upload a payload artifact) and is not
        // forwarded by the sampler. Drop it before we send.
        request.trace.take();

        let echo_agent_headers = EchoAgentRequestHeaders {
            conv_id: x_echo_agent_conv_id,
            req_id: x_echo_agent_req_id,
            model_id: &model_id,
            session_id: request
                .x_echo_agent_session_id
                .as_deref()
                .unwrap_or_default(),
            turn_idx: request.x_echo_agent_turn_idx.as_deref(),
            agent_id: request.x_echo_agent_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_echo_agent_deployment_id.as_deref(),
            user_id: request.x_echo_agent_user_id.as_deref(),
        };
        let extra_tool_entries = std::mem::take(&mut request.extra_tool_entries);
        let mut request_body = serde_json::to_value(&request.inner).map_err(|e| {
            tracing::error!("Failed to serialize responses request: {}", e);
            SamplingError::InvalidConfiguration("failed to serialize Responses request")
        })?;
        splice_extra_tool_entries(&mut request_body, extra_tool_entries);
        append_response_includes(&mut request_body, &self.defaults.extra_response_includes);
        // async-openai's ReasoningTextContent struct omits the `type`
        // discriminator that the Responses API requires on input. Patch
        // it in post-serialize. This is the last surviving piece of the
        // old raw_output machinery.
        echo_agent_sampling_types::patch_reasoning_text_types(&mut request_body);
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post(self.endpoint("responses"));
        let http_request = echo_agent_headers.apply(builder).json(&request_body);

        let response = http_request.send().await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            e
        })?;

        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = response.bytes().await?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::Responses,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("responses");
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let message = user_facing_api_error_message(status, bytes.as_ref());
            tracing::warn!(
                status = %status,
                error_message = %message,
                payload_bytes = bytes.len(),
                model_id = %model_id,
                "responses API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
                error_code: parse_error_code(bytes.as_ref()),
            });
        }

        let response_obj = serde_json::from_slice::<rs::Response>(&bytes).map_err(|e| {
            tracing::error!(
                error = %e,
                payload_bytes = bytes.len(),
                "Failed to deserialize rs::Response"
            );
            SamplingError::Serialization(e)
        })?;
        Ok(response_obj)
    }

    /// Create a streaming response using the Responses API.
    ///
    /// Returns a stream of `rs::ResponseStreamEvent` which includes events like:
    /// - `response.created` - Initial response object
    /// - `response.output_text.delta` - Text content deltas
    /// - `response.function_call_arguments.delta` - Function call argument deltas
    /// - `response.completed` - Final response with all output
    ///
    /// The third tuple element is a per-request doom-loop signal collector,
    /// `Some` only when `SamplerConfig::doom_loop_recovery` is set — the same
    /// gate that adds the opt-in `x-echo-agent-doom-loop-check` request header, so
    /// header and parse protection cannot drift apart. It is filled by the
    /// SSE decoder as the server reports triggers and is meant to be handed
    /// to `stream_responses` so the signals land on the final
    /// `ConversationResponse`.
    #[tracing::instrument(
        name = "http.create_response_stream",
        skip_all,
        fields(
            model_id = request.inner.model.as_deref().unwrap_or(""),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    #[allow(clippy::type_complexity)]
    pub async fn create_response_stream(
        &self,
        mut request: CreateResponseWrapper,
    ) -> Result<(
        BoxStream<'static, Result<rs::ResponseStreamEvent>>,
        Option<ResponseModelMetadata>,
        Option<crate::doom_loop::DoomLoopSignalCollector>,
    )> {
        self.apply_response_defaults(&mut request)?;

        // Enable streaming
        request.inner.stream = Some(true);

        let x_echo_agent_conv_id = request.x_echo_agent_conv_id.as_deref().unwrap_or_default();
        let x_echo_agent_req_id = request.x_echo_agent_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone().unwrap_or_default();

        // Drop process-local trace data (see note in `create_response`).
        request.trace.take();

        tracing::debug!(
            model_id = model_id.as_str(),
            "Sending responses API stream request"
        );

        let echo_agent_headers = EchoAgentRequestHeaders {
            conv_id: x_echo_agent_conv_id,
            req_id: x_echo_agent_req_id,
            model_id: &model_id,
            session_id: request
                .x_echo_agent_session_id
                .as_deref()
                .unwrap_or_default(),
            turn_idx: request.x_echo_agent_turn_idx.as_deref(),
            agent_id: request.x_echo_agent_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_echo_agent_deployment_id.as_deref(),
            user_id: request.x_echo_agent_user_id.as_deref(),
        };
        let extra_tool_entries = std::mem::take(&mut request.extra_tool_entries);
        let mut request_body = serde_json::to_value(&request.inner).map_err(|e| {
            tracing::error!("Failed to serialize responses request: {}", e);
            SamplingError::InvalidConfiguration("failed to serialize Responses request")
        })?;
        // Inject EchoAgent-specific fields not in async-openai's CreateResponse type.
        if self.defaults.stream_tool_calls {
            request_body["stream_tool_calls"] = serde_json::json!(true);
        }
        splice_extra_tool_entries(&mut request_body, extra_tool_entries);
        append_response_includes(&mut request_body, &self.defaults.extra_response_includes);
        echo_agent_sampling_types::patch_reasoning_text_types(&mut request_body);
        // Fresh per attempt so signals never leak across retries; `None`
        // (check disabled) sends no header and does no peek work per event.
        let doom_loop = self
            .defaults
            .doom_loop_recovery
            .map(crate::doom_loop::DoomLoopSignalCollector::new);
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post(self.endpoint("responses"));
        let mut http_request = echo_agent_headers
            .apply(builder)
            .header(ACCEPT, HeaderValue::from_static("text/event-stream"));
        if let Some(policy) = self.defaults.doom_loop_recovery {
            http_request =
                http_request.header(DOOM_LOOP_CHECK_HEADER, policy.window_tokens.to_string());
        }
        let http_request = http_request.json(&request_body);

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;

        tracing::debug!(
            url = %built_request.url(),
            method = %built_request.method(),
            "Sending responses API stream request"
        );
        Self::log_request_headers(&built_request, "responses");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::ResponsesStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("responses");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }
            let model_metadata = extract_model_metadata(response.headers());
            let retry_after_secs = extract_retry_after(response.headers());
            let should_retry = extract_should_retry(response.headers());
            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                payload_bytes = bytes.len(),
                model_id = %model_id,
                "responses API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
                error_code: parse_error_code(bytes.as_ref()),
            });
        }

        let model_metadata = extract_model_metadata(response.headers());

        // Strip UTF-8 BOM if present
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        let doom_loop_for_stream = doom_loop.clone();

        // The scan item is an `Option`: `Some(None)` skips an absorbed
        // doom-loop event without terminating the stream (`filter_map`
        // below), while an outer `None` still ends it.
        let events = event_stream
            .scan(false, move |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = &event.data;
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }

                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "responses",
                            payload_bytes = data.len(),
                        );

                        // Intercept the non-standard doom-loop event before
                        // typed deserialization; async-openai's event enum
                        // does not know it and would fail to parse it. With
                        // the check disabled, the shared name-or-payload-type
                        // predicate guards against a server emitting it
                        // despite no opt-in (rollout skew), named or not.
                        let swallow = match &doom_loop_for_stream {
                            Some(collector) => collector.absorb(&event.event, data),
                            None => is_check_event(&event.event, data),
                        };
                        if swallow {
                            Some(None)
                        } else if let Some(stream_error) = try_parse_stream_error(data) {
                            Some(Some(Err(stream_error)))
                        } else {
                            Some(Some(deserialize_response_event(data)))
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Some(Err(SamplingError::EventStreamError(e.to_string()))))
                    }
                };
                std::future::ready(item)
            })
            .filter_map(std::future::ready)
            .boxed();

        Ok((events, model_metadata, doom_loop))
    }

    // =========================================================================
    // Anthropic Messages API
    // =========================================================================

    /// Apply default configuration to a Messages API request.
    fn apply_message_defaults(&self, request: &mut MessagesRequestWrapper) -> Result<()> {
        // Apply model default if not specified
        if request.inner.model.is_empty() {
            request.inner.model = self.defaults.model.clone();
        }

        if request.inner.max_tokens == 0 {
            request.inner.max_tokens = self
                .defaults
                .max_completion_tokens
                .unwrap_or(ANTHROPIC_DEFAULT_MAX_TOKENS);
        }

        // Apply temperature default if not specified
        if request.inner.temperature.is_none() {
            request.inner.temperature = self.defaults.temperature;
        }

        // Apply top_p default if not specified
        if request.inner.top_p.is_none() {
            request.inner.top_p = self.defaults.top_p;
        }

        Ok(())
    }

    /// Create a message using the Anthropic Messages API (non-streaming).
    pub async fn create_message(
        &self,
        mut request: MessagesRequestWrapper,
    ) -> Result<messages::MessagesResponse> {
        self.apply_message_defaults(&mut request)?;

        let x_echo_agent_conv_id = request.x_echo_agent_conv_id.as_deref().unwrap_or_default();
        let x_echo_agent_req_id = request.x_echo_agent_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone();

        // Drop process-local trace data.
        request.trace.take();

        tracing::debug!("create_message: {:?}", &request.inner);
        tracing::debug!("endpoint: {:?}", self.endpoint("messages"));

        let echo_agent_headers = EchoAgentRequestHeaders {
            conv_id: x_echo_agent_conv_id,
            req_id: x_echo_agent_req_id,
            model_id: &model_id,
            session_id: request
                .x_echo_agent_session_id
                .as_deref()
                .unwrap_or_default(),
            turn_idx: request.x_echo_agent_turn_idx.as_deref(),
            agent_id: request.x_echo_agent_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_echo_agent_deployment_id.as_deref(),
            user_id: request.x_echo_agent_user_id.as_deref(),
        };
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post(self.endpoint("messages"));
        let http_request = echo_agent_headers.apply(builder).json(&request.inner);

        let response = http_request.send().await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            e
        })?;

        let status = response.status();
        let model_metadata = extract_model_metadata(response.headers());
        let retry_after_secs = extract_retry_after(response.headers());
        let should_retry = extract_should_retry(response.headers());
        let bytes = response.bytes().await?;

        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::Messages,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("messages");
                let server_message = user_facing_api_error_message(status, bytes.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }

            let message = user_facing_api_error_message(status, bytes.as_ref());
            tracing::warn!(
                status = %status,
                error_message = %message,
                payload_bytes = bytes.len(),
                model_id = %model_id,
                "messages API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
                error_code: parse_error_code(bytes.as_ref()),
            });
        }

        let response_obj =
            serde_json::from_slice::<messages::MessagesResponse>(&bytes).map_err(|e| {
                tracing::error!(
                    error = %e,
                    payload_bytes = bytes.len(),
                    "Failed to deserialize MessagesResponse"
                );
                SamplingError::Serialization(e)
            })?;
        Ok(response_obj)
    }

    /// Create a streaming message using the Anthropic Messages API.
    ///
    /// Returns a stream of `MessageStreamEvent` which includes events like:
    /// - `message_start` - Initial message object
    /// - `content_block_start` / `content_block_delta` / `content_block_stop` - Content blocks
    /// - `message_delta` / `message_stop` - Final message with stop reason
    #[tracing::instrument(
        name = "http.create_message_stream",
        skip_all,
        fields(
            model_id = request.inner.model.as_str(),
            status_code = tracing::field::Empty,
            success = tracing::field::Empty,
            error = tracing::field::Empty,
        )
    )]
    pub async fn create_message_stream(
        &self,
        mut request: MessagesRequestWrapper,
    ) -> Result<(
        BoxStream<'static, Result<messages::MessageStreamEvent>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_message_defaults(&mut request)?;

        // Enable streaming
        request.inner.stream = Some(true);

        let x_echo_agent_conv_id = request.x_echo_agent_conv_id.as_deref().unwrap_or_default();
        let x_echo_agent_req_id = request.x_echo_agent_req_id.as_deref().unwrap_or_default();
        let model_id = request.inner.model.clone();

        // Drop process-local trace data.
        request.trace.take();

        tracing::debug!(
            model_id = model_id.as_str(),
            "Sending Messages API stream request"
        );

        let echo_agent_headers = EchoAgentRequestHeaders {
            conv_id: x_echo_agent_conv_id,
            req_id: x_echo_agent_req_id,
            model_id: &model_id,
            session_id: request
                .x_echo_agent_session_id
                .as_deref()
                .unwrap_or_default(),
            turn_idx: request.x_echo_agent_turn_idx.as_deref(),
            agent_id: request.x_echo_agent_agent_id.as_deref().unwrap_or_default(),
            deployment_id: request.x_echo_agent_deployment_id.as_deref(),
            user_id: request.x_echo_agent_user_id.as_deref(),
        };
        let SentRequest {
            builder,
            sent_bearer,
        } = self.post(self.endpoint("messages"));
        let http_request = echo_agent_headers
            .apply(builder)
            .header(ACCEPT, HeaderValue::from_static("text/event-stream"))
            .json(&request.inner);

        let built_request = http_request.build().map_err(|e| {
            tracing::error!("Failed to build HTTP request: {}", e);
            SamplingError::Http(e)
        })?;

        tracing::debug!(
            url = %built_request.url(),
            method = %built_request.method(),
            "Sending messages API stream request"
        );
        Self::log_request_headers(&built_request, "messages");

        let response = self.http.execute(built_request).await.map_err(|e| {
            tracing::debug!("HTTP request failed: {}", e);
            record_stream_request_failure(&e);
            e
        })?;

        let status = response.status();
        let span = tracing::Span::current();
        span.record("status_code", status.as_u16() as i64);
        span.record("success", status.is_success());
        if !status.is_success() {
            if status == reqwest::StatusCode::UNAUTHORIZED {
                span.record("error", "unauthorized (401)");
                self.record_401_attribution(
                    crate::attribution::SamplingConsumer::MessagesStream,
                    sent_bearer.as_deref(),
                );
                let endpoint = self.endpoint("messages");
                let body = response.bytes().await.unwrap_or_default();
                let server_message = user_facing_api_error_message(status, body.as_ref());
                return Err(auth_rejected(
                    format!("Unauthorized (401) from {endpoint}: {server_message}"),
                    sent_bearer.as_deref(),
                ));
            }
            let model_metadata = extract_model_metadata(response.headers());
            let retry_after_secs = extract_retry_after(response.headers());
            let should_retry = extract_should_retry(response.headers());
            let bytes = response.bytes().await?;
            let message = user_facing_api_error_message(status, bytes.as_ref());
            span.record("error", message.as_str());
            tracing::error!(
                status = %status,
                error_message = %message,
                payload_bytes = bytes.len(),
                model_id = %model_id,
                "messages API error"
            );
            return Err(SamplingError::Api {
                status,
                message,
                model_metadata,
                retry_after_secs,
                should_retry,
                error_code: parse_error_code(bytes.as_ref()),
            });
        }

        let model_metadata = extract_model_metadata(response.headers());

        // Strip UTF-8 BOM if present
        const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
        let mut is_first = true;
        let byte_stream = response.bytes_stream().map(move |result| {
            result.map(|bytes| {
                if is_first {
                    is_first = false;
                    if bytes.starts_with(UTF8_BOM) {
                        return bytes.slice(UTF8_BOM.len()..);
                    }
                }
                bytes
            })
        });

        // Turn raw bytes into SSE events
        let event_stream = byte_stream.eventsource();

        // Map SSE events into MessageStreamEvent.
        // Uses `scan` so transport errors terminate the stream after the first
        // error (same pattern as `chat_completion_stream`).
        let events = event_stream
            .scan(false, |had_transport_error, event_res| {
                if *had_transport_error {
                    return std::future::ready(None);
                }
                let item = match event_res {
                    Ok(event) => {
                        let data = &event.data;
                        if data == "[DONE]" {
                            return std::future::ready(None);
                        }

                        tracing::info!(
                            target: crate::sampling_log::TARGET,
                            event = "sse_chunk",
                            backend = "messages",
                            payload_bytes = data.len(),
                        );

                        if let Some(stream_error) = try_parse_stream_error(data) {
                            Some(Err(stream_error))
                        } else {
                            Some(
                                serde_json::from_str::<messages::MessageStreamEvent>(data).map_err(
                                    |e| {
                                        tracing::error!(
                                            error = %e,
                                            payload_bytes = data.len(),
                                            "Failed to deserialize MessageStreamEvent from stream"
                                        );
                                        SamplingError::Serialization(e)
                                    },
                                ),
                            )
                        }
                    }
                    Err(e) => {
                        *had_transport_error = true;
                        Some(Err(SamplingError::EventStreamError(e.to_string())))
                    }
                };
                std::future::ready(item)
            })
            .boxed();

        Ok((events, model_metadata))
    }

    // =========================================================================
    // Unified Conversation API
    // =========================================================================

    /// Apply default configuration to a ConversationRequest.
    fn apply_conversation_defaults(&self, request: &mut ConversationRequest) -> Result<()> {
        if let Some(policy) = request.preflight.clone() {
            policy
                .apply(request)
                .map_err(SamplingError::InvalidConfiguration)?;
        }
        if request.model.is_none() {
            request.model = Some(self.defaults.model.clone());
        }

        if request.temperature.is_none() {
            request.temperature = self.defaults.temperature;
        }

        if request.top_p.is_none() {
            request.top_p = self.defaults.top_p;
        }

        if request.max_output_tokens.is_none() {
            request.max_output_tokens = self.defaults.max_completion_tokens;
        }

        Ok(())
    }

    /// Send a conversation request using the Chat Completions API (streaming).
    ///
    /// Converts the `ConversationRequest` to `ChatCompletionRequest` internally.
    /// Returns the stream and any model metadata extracted from response headers.
    pub async fn conversation_stream(
        &self,
        request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<ChatCompletionChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        self.conversation_stream_with_parallel_tool_calls(request, None)
            .await
    }

    /// Chat Completions stream with a request-local parallel tool-call policy.
    /// Used only for safe recovery from providers whose streamed tool indices
    /// are ambiguous; the caller's conversation and global defaults stay
    /// unchanged.
    pub(crate) async fn conversation_stream_with_parallel_tool_calls(
        &self,
        mut request: ConversationRequest,
        parallel_tool_calls: Option<bool>,
    ) -> Result<(
        BoxStream<'static, Result<ChatCompletionChunk>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let mut chat_request: ChatCompletionRequest = request.into();
        chat_request.parallel_tool_calls = parallel_tool_calls;
        if let Some(trace) = trace {
            chat_request.trace = Some(trace);
        }

        self.chat_completion_stream(chat_request).await
    }

    /// Send a conversation request using the Chat Completions API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to `ChatCompletionRequest` internally.
    pub async fn conversation(
        &self,
        mut request: ConversationRequest,
    ) -> Result<ChatCompletionResponse> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let mut chat_request: ChatCompletionRequest = request.into();
        if let Some(trace) = trace {
            chat_request.trace = Some(trace);
        }

        self.chat_completion(chat_request).await
    }

    /// Send a conversation request using the Responses API (streaming).
    ///
    /// Converts the `ConversationRequest` to Responses API format internally.
    /// The third tuple element is the per-request doom-loop signal collector
    /// (see [`Self::create_response_stream`]); callers that don't consume the
    /// signals can ignore it.
    #[allow(clippy::type_complexity)]
    pub async fn conversation_stream_responses(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<rs::ResponseStreamEvent>>,
        Option<ResponseModelMetadata>,
        Option<crate::doom_loop::DoomLoopSignalCollector>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_echo_agent_conv_id = request.x_echo_agent_conv_id.clone();
        let x_echo_agent_req_id = request.x_echo_agent_req_id.clone();
        let x_echo_agent_session_id = request.x_echo_agent_session_id.clone();
        let x_echo_agent_turn_idx = request.x_echo_agent_turn_idx.clone();
        let x_echo_agent_agent_id = request.x_echo_agent_agent_id.clone();

        // The hosted tools travel as raw JSON, spliced in after serialization by
        // `splice_extra_tool_entries`, whose doc explains why each one does.
        let extra_tools = echo_agent_sampling_types::extra_tool_entries(&request.hosted_tools);

        let responses_request: rs::CreateResponse = (&request).into();

        let mut wrapper = CreateResponseWrapper::new(responses_request);
        wrapper.x_echo_agent_conv_id = x_echo_agent_conv_id;
        wrapper.x_echo_agent_req_id = x_echo_agent_req_id;
        wrapper.x_echo_agent_session_id = x_echo_agent_session_id;
        wrapper.x_echo_agent_turn_idx = x_echo_agent_turn_idx;
        wrapper.x_echo_agent_agent_id = x_echo_agent_agent_id;
        wrapper.extra_tool_entries = extra_tools;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_response_stream(wrapper).await
    }

    /// Send a conversation request using the Responses API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to Responses API format internally.
    pub async fn conversation_responses(
        &self,
        mut request: ConversationRequest,
    ) -> Result<rs::Response> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_echo_agent_conv_id = request.x_echo_agent_conv_id.clone();
        let x_echo_agent_req_id = request.x_echo_agent_req_id.clone();
        let x_echo_agent_session_id = request.x_echo_agent_session_id.clone();
        let x_echo_agent_turn_idx = request.x_echo_agent_turn_idx.clone();
        let x_echo_agent_agent_id = request.x_echo_agent_agent_id.clone();

        // The hosted tools travel as raw JSON, spliced in by `create_response` through
        // `splice_extra_tool_entries`, whose doc explains why each one does.
        let extra_tools = echo_agent_sampling_types::extra_tool_entries(&request.hosted_tools);

        let responses_request: rs::CreateResponse = (&request).into();

        let mut wrapper = CreateResponseWrapper::new(responses_request);
        wrapper.x_echo_agent_conv_id = x_echo_agent_conv_id;
        wrapper.x_echo_agent_req_id = x_echo_agent_req_id;
        wrapper.x_echo_agent_session_id = x_echo_agent_session_id;
        wrapper.x_echo_agent_turn_idx = x_echo_agent_turn_idx;
        wrapper.x_echo_agent_agent_id = x_echo_agent_agent_id;
        wrapper.extra_tool_entries = extra_tools;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_response(wrapper).await
    }

    /// Send a conversation request using the Anthropic Messages API (streaming).
    ///
    /// Converts the `ConversationRequest` to Messages API format internally.
    pub async fn conversation_stream_messages(
        &self,
        mut request: ConversationRequest,
    ) -> Result<(
        BoxStream<'static, Result<messages::MessageStreamEvent>>,
        Option<ResponseModelMetadata>,
    )> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_echo_agent_conv_id = request.x_echo_agent_conv_id.clone();
        let x_echo_agent_req_id = request.x_echo_agent_req_id.clone();
        let x_echo_agent_session_id = request.x_echo_agent_session_id.clone();
        let x_echo_agent_turn_idx = request.x_echo_agent_turn_idx.clone();
        let x_echo_agent_agent_id = request.x_echo_agent_agent_id.clone();

        let messages_request = build_messages_request(&request);

        let mut wrapper = MessagesRequestWrapper::new(messages_request);
        wrapper.x_echo_agent_conv_id = x_echo_agent_conv_id;
        wrapper.x_echo_agent_req_id = x_echo_agent_req_id;
        wrapper.x_echo_agent_session_id = x_echo_agent_session_id;
        wrapper.x_echo_agent_turn_idx = x_echo_agent_turn_idx;
        wrapper.x_echo_agent_agent_id = x_echo_agent_agent_id;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_message_stream(wrapper).await
    }

    /// Send a conversation request using the Anthropic Messages API (non-streaming).
    ///
    /// Converts the `ConversationRequest` to Messages API format internally.
    pub async fn conversation_messages(
        &self,
        mut request: ConversationRequest,
    ) -> Result<messages::MessagesResponse> {
        self.apply_conversation_defaults(&mut request)?;

        let trace = request.trace.take();
        let x_echo_agent_conv_id = request.x_echo_agent_conv_id.clone();
        let x_echo_agent_req_id = request.x_echo_agent_req_id.clone();
        let x_echo_agent_session_id = request.x_echo_agent_session_id.clone();
        let x_echo_agent_turn_idx = request.x_echo_agent_turn_idx.clone();
        let x_echo_agent_agent_id = request.x_echo_agent_agent_id.clone();

        let messages_request = build_messages_request(&request);

        let mut wrapper = MessagesRequestWrapper::new(messages_request);
        wrapper.x_echo_agent_conv_id = x_echo_agent_conv_id;
        wrapper.x_echo_agent_req_id = x_echo_agent_req_id;
        wrapper.x_echo_agent_session_id = x_echo_agent_session_id;
        wrapper.x_echo_agent_turn_idx = x_echo_agent_turn_idx;
        wrapper.x_echo_agent_agent_id = x_echo_agent_agent_id;

        if let Some(trace) = trace {
            wrapper.trace = Some(trace);
        }

        self.create_message(wrapper).await
    }

    /// Backend-aware streaming call that collects the full response.
    pub async fn conversation_collect(
        &self,
        request: ConversationRequest,
    ) -> Result<ConversationResponse> {
        let request_id = crate::types::RequestId::random();
        let idle_timeout = std::time::Duration::from_secs(300);
        let result = match self.api_backend() {
            ApiBackend::ChatCompletions => {
                let (raw, meta) = self.conversation_stream(request).await?;
                let events =
                    crate::stream::stream_chat_completions(raw, meta, request_id, idle_timeout);
                crate::stream::collect_response(events).await
            }
            ApiBackend::Responses => {
                let (raw, meta, doom_loop) = self.conversation_stream_responses(request).await?;
                let events =
                    crate::stream::stream_responses(raw, meta, request_id, idle_timeout, doom_loop);
                crate::stream::collect_response(events).await
            }
            ApiBackend::Messages => {
                let (raw, meta) = self.conversation_stream_messages(request).await?;
                let events = crate::stream::stream_messages(raw, meta, request_id, idle_timeout);
                crate::stream::collect_response(events).await
            }
        };
        result
            .map(|(response, _metrics)| response)
            .map_err(stream_collect_error)
    }
}

/// Preserve stream interruption and truncation semantics for one-shot callers.
/// Other failures retain their status, `Retry-After` and `x-should-retry`.
fn stream_collect_error(info: SamplingErrorInfo) -> SamplingError {
    match info.kind {
        SamplingErrorKind::MaxTokensTruncation => return SamplingError::MaxTokensTruncation,
        SamplingErrorKind::ChatStreamInterrupted => {
            return SamplingError::ChatStreamInterrupted {
                detail: info.message,
                retryable: info.is_retryable,
            };
        }
        _ => {}
    }
    SamplingError::Api {
        status: info
            .status_code
            .and_then(|c| reqwest::StatusCode::from_u16(c).ok())
            .unwrap_or(reqwest::StatusCode::INTERNAL_SERVER_ERROR),
        message: info.message,
        model_metadata: info.model_metadata,
        retry_after_secs: info.retry_after_secs,
        should_retry: info.should_retry,
        error_code: info.error_code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Bytes, routing::post};
    use echo_agent_sampling_types::ApiErrorCode;
    use echo_agent_sampling_types::types::ChatRequestMessage;
    use indexmap::IndexMap;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    #[test]
    fn splice_extra_tool_entries_extends_existing_tools_array() {
        let mut body = serde_json::json!({ "tools": [{ "type": "function" }] });
        splice_extra_tool_entries(&mut body, vec![serde_json::json!({ "type": "web_search" })]);
        assert_eq!(
            body["tools"],
            serde_json::json!([{ "type": "function" }, { "type": "web_search" }])
        );
    }

    #[test]
    fn splice_extra_tool_entries_creates_tools_array_when_absent() {
        let mut body = serde_json::json!({});
        splice_extra_tool_entries(&mut body, vec![serde_json::json!({ "type": "web_search" })]);
        assert_eq!(body["tools"], serde_json::json!([{ "type": "web_search" }]));
    }

    #[test]
    fn splice_extra_tool_entries_noop_when_empty() {
        let mut body = serde_json::json!({ "tools": [{ "type": "function" }] });
        splice_extra_tool_entries(&mut body, vec![]);
        assert_eq!(body["tools"], serde_json::json!([{ "type": "function" }]));
    }

    #[test]
    fn stream_collect_error_preserves_should_retry() {
        let info = SamplingErrorInfo {
            kind: crate::events::SamplingErrorKind::Api,
            status_code: Some(529),
            message: "Overloaded".into(),
            is_retryable: true,
            retry_after_secs: Some(3),
            should_retry: Some(false),
            error_code: Some(ApiErrorCode::InvalidImage),
            model_metadata: None,
            empty_response_context: None,
            doom_loop_triggers: None,
            doom_loop_aborted_at_chunk: None,
            credential: echo_agent_sampling_types::SentCredential::Unknown,
        };
        // SamplingError is not PartialEq (it carries reqwest/serde errors),
        // so destructure once and compare all fields in a single assert.
        let SamplingError::Api {
            status,
            message,
            model_metadata,
            retry_after_secs,
            should_retry,
            error_code,
        } = stream_collect_error(info)
        else {
            panic!("expected Api");
        };
        assert_eq!(
            (
                status.as_u16(),
                message.as_str(),
                model_metadata.is_none(),
                retry_after_secs,
                should_retry,
                error_code,
            ),
            (
                529,
                "Overloaded",
                true,
                Some(3),
                Some(false),
                Some(ApiErrorCode::InvalidImage)
            ),
        );
    }

    fn minimal_config() -> SamplerConfig {
        SamplerConfig {
            api_key: Some("test-key".to_string()),
            base_url: "https://example.test".to_string(),
            model: "test-model".to_string(),
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: ApiBackend::ChatCompletions,
            auth_scheme: AuthScheme::Bearer,
            extra_headers: IndexMap::new(),
            extra_response_includes: Vec::new(),
            query_params: IndexMap::new(),
            env_http_headers: IndexMap::new(),
            context_window: 8192,
            force_http1: false,
            max_retries: None,
            stream_tool_calls: false,
            idle_timeout_secs: None,
            reasoning_effort: None,
            origin_client: None,
            client_identifier: None,
            deployment_id: None,
            user_id: None,
            client_version: None,
            attribution_callback: None,
            bearer_resolver: None,
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            doom_loop_recovery: None,
            header_injector: None,
        }
    }

    /// Regression for OpenAI-compatible providers (including MiniMax) that
    /// encode an unfinished streaming choice as `finish_reason: ""` instead
    /// of the protocol-standard `null`. Exercise the real HTTP/SSE ingress so
    /// the field-level compatibility deserializer cannot be accidentally
    /// bypassed by a future client refactor.
    #[tokio::test]
    async fn chat_completion_stream_accepts_empty_finish_reason() {
        let body = concat!(
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",",
            "\"created\":1,\"model\":\"MiniMax-M3\",\"choices\":[{\"index\":0,",
            "\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},",
            "\"finish_reason\":\"\"}]}\n\n",
            "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",",
            "\"created\":1,\"model\":\"MiniMax-M3\",\"choices\":[{\"index\":0,",
            "\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || async move {
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = SamplingClient::new(SamplerConfig {
            base_url: format!("http://{addr}/v1"),
            model: "MiniMax-M3".into(),
            ..minimal_config()
        })
        .unwrap();
        let request =
            ChatCompletionRequest::new("MiniMax-M3", vec![ChatRequestMessage::user("hello")]);

        let (mut stream, _) = client
            .chat_completion_stream(request)
            .await
            .expect("the SSE request should start");
        let content_chunk = stream
            .next()
            .await
            .expect("content chunk")
            .expect("empty finish_reason must not fail deserialization");
        assert_eq!(
            content_chunk.choices[0].delta.content.as_deref(),
            Some("ok")
        );
        assert_eq!(content_chunk.choices[0].finish_reason, None);

        let final_chunk = stream.next().await.expect("final chunk").unwrap();
        assert_eq!(
            final_chunk.choices[0].finish_reason,
            Some(echo_agent_sampling_types::types::FinishReason::Stop)
        );
        assert!(
            stream.next().await.is_none(),
            "[DONE] must close the stream"
        );
        server.abort();
    }

    /// Verify the serialized shape of StreamingChatRequest matches the
    /// expected wire format: all ChatCompletionRequest fields flattened at
    /// top level, plus `stream: true` and `stream_options.include_usage: true`.
    #[test]
    fn streaming_chat_request_serializes_correctly() {
        let request = ChatCompletionRequest {
            model: Some("test-model".into()),
            messages: vec![ChatRequestMessage::user("hello")],
            temperature: Some(0.7),
            max_tokens: None,
            top_p: None,
            frequency_penalty: None,
            presence_penalty: None,
            user: None,
            tools: None,
            tool_choice: None,
            parallel_tool_calls: Some(false),
            search_parameters: None,
            response_format: None,
            reasoning_effort: None,
            x_echo_agent_conv_id: None,
            x_echo_agent_req_id: None,
            x_echo_agent_session_id: None,
            x_echo_agent_turn_idx: None,
            x_echo_agent_agent_id: None,
            x_echo_agent_deployment_id: None,
            x_echo_agent_user_id: None,
            trace: None,
        };

        let wrapper = StreamingChatRequest {
            inner: &request,
            stream: true,
            stream_options: StreamOptions {
                include_usage: true,
            },
        };

        let json: serde_json::Value = serde_json::to_value(&wrapper).unwrap();
        let obj = json.as_object().unwrap();

        assert_eq!(obj.get("stream").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(
            obj.get("parallel_tool_calls").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            obj.get("stream_options")
                .and_then(|v| v.get("include_usage"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );

        assert!(
            obj.get("inner").is_none(),
            "inner field should be flattened"
        );
        assert_eq!(
            obj.get("model").and_then(|v| v.as_str()),
            Some("test-model")
        );
        assert!(obj.get("messages").is_some());
        let temp = obj.get("temperature").and_then(|v| v.as_f64()).unwrap();
        assert!((temp - 0.7).abs() < 0.001, "temperature should be ~0.7");

        assert!(obj.get("max_tokens").is_none());
        assert!(obj.get("tools").is_none());
    }

    async fn capture_response_body(streaming: bool) -> serde_json::Value {
        let (body_tx, body_rx) = oneshot::channel();
        let body_tx = std::sync::Arc::new(std::sync::Mutex::new(Some(body_tx)));
        let app = Router::new().route(
            "/v1/responses",
            post(move |body: Bytes| {
                let body_tx = body_tx.clone();
                async move {
                    let _ = body_tx.lock().unwrap().take().unwrap().send(body);
                    if streaming {
                        axum::response::Response::builder()
                            .header("content-type", "text/event-stream")
                            .body(axum::body::Body::from("data: [DONE]\n\n"))
                            .unwrap()
                    } else {
                        axum::response::Response::builder()
                            .header("content-type", "application/json")
                            .body(axum::body::Body::from(r#"{"id":"resp","object":"response","created_at":0,"model":"test-model","status":"completed","output":[],"usage":{"input_tokens":0,"input_tokens_details":{"cached_tokens":0},"output_tokens":0,"output_tokens_details":{"reasoning_tokens":0},"total_tokens":0}}"#))
                            .unwrap()
                    }
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let client = SamplingClient::new(SamplerConfig {
            base_url: format!("http://{addr}/v1"),
            api_backend: ApiBackend::Responses,
            extra_response_includes: vec!["no_inline_citations".to_owned()],
            ..minimal_config()
        })
        .unwrap();
        let mut request = rs::CreateResponse {
            input: rs::InputParam::Text("hi".to_owned()),
            include: Some(vec![rs::IncludeEnum::ReasoningEncryptedContent]),
            tools: Some(vec![rs::Tool::WebSearch(rs::WebSearchTool::default())]),
            ..Default::default()
        };
        let mut wrapper = CreateResponseWrapper::new(request.clone());
        wrapper.extra_tool_entries = vec![serde_json::json!({"type": "x_search"})];
        if streaming {
            let (_stream, _model_metadata, _doom_loop_collector) = client
                .create_response_stream(wrapper)
                .await
                .expect("streaming request should succeed");
        } else {
            request.tools = None;
            client
                .create_response(CreateResponseWrapper::new(request))
                .await
                .expect("unary request should succeed");
        }
        let body = body_rx.await.unwrap();
        server.abort();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn response_call_sites_emit_final_includes_and_stream_fields() {
        let unary = capture_response_body(false).await;
        assert_eq!(
            serde_json::json!(["reasoning.encrypted_content", "no_inline_citations"]),
            unary["include"],
        );

        let stream = capture_response_body(true).await;
        assert_eq!(
            serde_json::json!(["reasoning.encrypted_content", "no_inline_citations"]),
            stream["include"],
        );
        assert_eq!(Some(true), stream["stream"].as_bool());
        assert!(
            stream["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["type"] == "x_search")
        );
    }

    #[test]
    fn append_response_includes_preserves_typed_values_and_deduplicates() {
        let typed = [
            "reasoning.encrypted_content",
            "web_search_call.action.sources",
        ];
        let mut body = serde_json::json!({ "include": typed });
        append_response_includes(
            &mut body,
            &[
                "no_inline_citations".to_owned(),
                "no_inline_citations".to_owned(),
            ],
        );
        assert_eq!(
            serde_json::json!([
                "reasoning.encrypted_content",
                "web_search_call.action.sources",
                "no_inline_citations",
            ]),
            body["include"],
        );

        let mut unchanged = serde_json::json!({ "include": typed });
        let expected = unchanged.clone();
        append_response_includes(&mut unchanged, &[]);
        assert_eq!(expected, unchanged);

        for mut body in [
            serde_json::json!({}),
            serde_json::json!({ "include": null }),
        ] {
            append_response_includes(&mut body, &["no_inline_citations".to_owned()]);
            assert_eq!(serde_json::json!(["no_inline_citations"]), body["include"]);
        }
    }

    #[test]
    fn extract_retry_after_parses_seconds() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "30".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(30));
    }

    #[test]
    fn extract_retry_after_caps_at_120() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "3600".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(120));
    }

    #[test]
    fn extract_retry_after_zero_is_valid() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::RETRY_AFTER, "0".parse().unwrap());
        assert_eq!(extract_retry_after(&headers), Some(0));
    }

    #[test]
    fn extract_retry_after_ignores_http_date() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            "Fri, 31 Dec 2025 23:59:59 GMT".parse().unwrap(),
        );
        assert_eq!(extract_retry_after(&headers), None);
    }

    #[test]
    fn extract_retry_after_none_when_missing() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(extract_retry_after(&headers), None);
    }

    #[test]
    fn extract_should_retry_true() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "true".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(true));
    }

    #[test]
    fn extract_should_retry_true_case_insensitive() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "TRUE".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(true));
    }

    #[test]
    fn extract_should_retry_false() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "false".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), Some(false));
    }

    #[test]
    fn extract_should_retry_unknown_value_is_none() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-should-retry", "banana".parse().unwrap());
        assert_eq!(extract_should_retry(&headers), None);
    }

    #[test]
    fn extract_should_retry_absent_is_none() {
        let headers = reqwest::header::HeaderMap::new();
        assert_eq!(extract_should_retry(&headers), None);
    }

    #[test]
    fn new_with_minimal_config_succeeds() {
        let client = SamplingClient::new(minimal_config()).expect("client should construct");
        assert_eq!(client.api_backend(), ApiBackend::ChatCompletions);
    }

    #[test]
    fn secure_provider_url_policy_rejects_remote_http_and_gates_loopback() {
        assert!(
            validate_sampling_base_url_with_policy("https://api.example.com/v1", true, false)
                .is_ok()
        );
        assert!(
            validate_sampling_base_url_with_policy("http://api.example.com/v1", true, true)
                .is_err()
        );
        assert!(
            validate_sampling_base_url_with_approval(
                "http://api.example.com:60100/v1",
                true,
                false,
                true,
            )
            .is_ok()
        );
        assert!(
            validate_sampling_base_url_with_approval(
                "http://api.example.com:60100/other",
                true,
                false,
                false,
            )
            .is_err()
        );
        assert!(
            validate_sampling_base_url_with_policy("http://123.56.188.16:8088/v1", true, false)
                .is_ok()
        );
        assert!(
            validate_sampling_base_url_with_policy(
                "http://123.56.188.16.evil:8088/v1",
                true,
                false
            )
            .is_err()
        );
        assert!(
            validate_sampling_base_url_with_policy("http://127.0.0.1:11434/v1", true, false)
                .is_err()
        );
        assert!(
            validate_sampling_base_url_with_policy("http://127.0.0.1:11434/v1", true, true).is_ok()
        );
        assert!(
            validate_sampling_base_url_with_policy("http://localhost:11434/v1", true, true).is_ok()
        );
        assert!(
            validate_sampling_base_url_with_policy("http://[::1]:11434/v1", true, true).is_ok()
        );
        assert!(
            validate_sampling_base_url_with_policy(
                "https://user:pass@api.example.com/v1",
                true,
                false
            )
            .is_err()
        );
        // Upstream CLI consumers retain their existing behavior unless native
        // EchoAgent startup explicitly enables this policy.
        assert!(
            validate_sampling_base_url_with_policy("http://api.example.com/v1", false, false)
                .is_ok()
        );
    }

    #[test]
    fn desktop_approval_is_scoped_to_one_http_base_url() {
        replace_approved_insecure_http_urls(["http://approved.example:60100/v1".to_owned()]);
        assert!(
            validate_sampling_base_url_with_policy("http://approved.example:60100/v1", true, false)
                .is_ok()
        );
        assert!(
            validate_sampling_base_url_with_policy(
                "http://approved.example:60100/other",
                true,
                false
            )
            .is_err()
        );
        replace_approved_insecure_http_urls(std::iter::empty());
        assert!(
            validate_sampling_base_url_with_policy("http://approved.example:60100/v1", true, false)
                .is_err()
        );
    }

    #[test]
    fn new_applies_extra_headers() {
        let mut cfg = minimal_config();
        cfg.extra_headers
            .insert("x-test-header".to_string(), "test-value".to_string());
        cfg.extra_headers.insert(
            "x-echo-agent-token-auth".to_string(),
            "echo-agent-cli".to_string(),
        );
        let _client = SamplingClient::new(cfg).expect("client with extra headers should construct");
    }

    #[test]
    fn apply_env_http_headers_resolves_trims_skips_and_overrides() {
        let mut map = IndexMap::new();
        map.insert("x-tenant-token".to_string(), "TENANT".to_string());
        map.insert("x-blank".to_string(), "BLANK".to_string());
        map.insert("x-missing".to_string(), "MISSING".to_string());
        map.insert("x-override".to_string(), "OVERRIDE".to_string());
        map.insert("x invalid".to_string(), "INVALID".to_string());

        let mut headers = HeaderMap::new();
        headers.insert(
            HeaderName::from_static("x-override"),
            HeaderValue::from_static("static"),
        );

        apply_env_http_headers(
            &map,
            |var| match var {
                // Leading space + trailing newline exercises trimming.
                "TENANT" => Some(" tenant-secret\n".to_string()),
                "BLANK" => Some("   ".to_string()),
                "OVERRIDE" => Some("from-env".to_string()),
                "INVALID" => Some("value".to_string()),
                _ => None,
            },
            &mut headers,
        );

        assert_eq!(headers.get("x-tenant-token").unwrap(), "tenant-secret");
        assert!(headers.get("x-blank").is_none());
        assert!(headers.get("x-missing").is_none());
        // A resolved env value overrides an existing header of the same name.
        assert_eq!(headers.get("x-override").unwrap(), "from-env");
        // An invalid header name is skipped rather than panicking.
        assert!(headers.get("x invalid").is_none());
    }

    #[test]
    fn endpoint_appends_path_before_a_base_url_query_without_configured_params() {
        let template =
            EndpointTemplate::new("https://gateway.example/v1?api-version=x", &IndexMap::new());
        let url = template.url_for_path("responses");
        assert!(
            url.starts_with("https://gateway.example/v1/responses?"),
            "url: {url}"
        );
        assert!(url.contains("api-version=x"), "url: {url}");
        assert!(!url.contains("x/responses"), "url: {url}");
    }

    #[test]
    fn messages_plus_anthropic_api_key_uses_x_api_key_and_not_authorization() {
        let cfg = SamplerConfig {
            api_key: Some("anthropic-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert!(
            client
                .default_headers
                .get(HeaderName::from_static("x-api-key"))
                .is_some()
        );
        assert!(client.default_headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn messages_plus_bearer_uses_authorization_and_not_x_api_key() {
        let cfg = SamplerConfig {
            api_key: Some("bearer-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::Bearer,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert!(client.default_headers.get(AUTHORIZATION).is_some());
        assert!(
            client
                .default_headers
                .get(HeaderName::from_static("x-api-key"))
                .is_none()
        );
    }

    // Regression: a past change dropped User-Agent from sampling requests.
    #[test]
    fn sampling_client_always_has_user_agent() {
        let client = SamplingClient::new(minimal_config()).expect("build");
        assert!(client.default_headers.contains_key(USER_AGENT));
    }

    // Regression: a past change dropped HeaderInjector (traceparent) from sampling requests.
    #[test]
    fn header_injector_is_called_in_post() {
        #[derive(Debug)]
        struct TestInjector;
        impl crate::config::HeaderInjector for TestInjector {
            fn inject(&self, headers: &mut HeaderMap) {
                headers.insert(
                    HeaderName::from_static("traceparent"),
                    HeaderValue::from_static("00-test-trace-id-00"),
                );
            }
        }

        let mut config = minimal_config();
        config.header_injector = Some(std::sync::Arc::new(TestInjector));
        let client = SamplingClient::new(config).expect("build");
        let SentRequest { builder, .. } = client.post("http://localhost/test");
        let req = builder.build().expect("build request");
        assert!(
            req.headers().contains_key("traceparent"),
            "HeaderInjector should inject traceparent into post() requests"
        );
    }

    #[test]
    fn user_agent_includes_origin_and_agent_product() {
        let origin = OriginClientInfo {
            product: "my-client".to_string(),
            version: Some("1.2.3".to_string()),
        };
        let ua = user_agent_string_for(&origin);
        assert!(ua.contains("my-client/1.2.3"));
        assert!(ua.contains(AGENT_PRODUCT));
    }

    #[test]
    fn user_agent_omits_origin_version_when_absent() {
        let origin = OriginClientInfo {
            product: "my-client".to_string(),
            version: None,
        };
        let ua = user_agent_string_for(&origin);
        // No slash between product and the echo-agent-shell agent product.
        assert!(ua.starts_with("my-client echo-agent-shell/"));
    }

    #[test]
    fn user_agent_collapses_when_origin_matches_agent() {
        let agent_version = echo_agent_version::VERSION.to_string();
        let origin = OriginClientInfo {
            product: AGENT_PRODUCT.to_string(),
            version: Some(agent_version.clone()),
        };
        let ua = user_agent_string_for(&origin);
        // Single product/version slot when the origin and agent match.
        assert!(ua.starts_with(&format!("{}/{}", AGENT_PRODUCT, agent_version)));
    }

    /// Counts callbacks for assertions in the tests below.
    #[derive(Default, Debug)]
    struct CountingCallback {
        invocations: std::sync::Mutex<Vec<(crate::attribution::SamplingConsumer, Option<String>)>>,
    }

    #[derive(Debug)]
    struct StaticBearerResolver(&'static str);

    impl crate::config::BearerResolver for StaticBearerResolver {
        fn current_bearer(&self) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    impl crate::attribution::Auth401AttributionCallback for CountingCallback {
        fn record_401(
            &self,
            consumer: crate::attribution::SamplingConsumer,
            sent_bearer: Option<&str>,
        ) {
            self.invocations
                .lock()
                .unwrap()
                .push((consumer, sent_bearer.map(|s| s.to_string())));
        }
    }

    /// `post()` strips the `"Bearer "` scheme prefix off `Authorization`
    /// and captures the tail fragment (see `BEARER_SUFFIX_LEN`).
    #[test]
    fn post_captures_bearer_tail_for_openai_compat() {
        let cfg = SamplerConfig {
            api_key: Some("test-bearer-1234567890".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/chat/completions");
        assert_eq!(bearer.as_deref(), Some("r-1234567890"));
        assert_eq!(
            bearer.as_deref().map(str::len),
            Some(crate::attribution::BEARER_SUFFIX_LEN),
        );
    }

    /// `post()` captures `x-api-key` for Messages-API backends and keeps
    /// the value's tail fragment.
    #[test]
    fn post_captures_x_api_key_tail_for_messages() {
        let cfg = SamplerConfig {
            api_key: Some("anthropic-key-abc123".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/messages");
        assert_eq!(bearer.as_deref(), Some("c-key-abc123"));
        assert_eq!(
            bearer.as_deref().map(str::len),
            Some(crate::attribution::BEARER_SUFFIX_LEN),
        );
    }

    /// `post()` captures `None` when the request carries no auth header.
    #[test]
    fn post_captures_none_when_no_header() {
        let cfg = SamplerConfig {
            api_key: None,
            api_backend: ApiBackend::ChatCompletions,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            sent_bearer: bearer,
            ..
        } = client.post("https://example.test/v1/chat/completions");
        assert!(bearer.is_none());
    }

    /// The race this design closes: a 401 triggers a recovery that rotates
    /// the resolver, so a record-time re-read attributes a bearer the
    /// rejected request never carried. The attributed fragment must be the
    /// one captured when the request was built.
    #[test]
    fn post_capture_is_immune_to_resolver_rotation_after_build() {
        #[derive(Debug)]
        struct RotatingResolver(std::sync::Mutex<String>);
        impl crate::config::BearerResolver for RotatingResolver {
            fn current_bearer(&self) -> Option<String> {
                Some(self.0.lock().unwrap().clone())
            }
        }

        let resolver = std::sync::Arc::new(RotatingResolver(std::sync::Mutex::new(
            "rejected-token-oldtail1".to_string(),
        )));
        let cfg = SamplerConfig {
            api_key: None,
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(resolver.clone()),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");

        let SentRequest {
            sent_bearer: sent_at_build,
            ..
        } = client.post("https://example.test/v1/responses");
        // The 401 kicks recovery; the resolver rotates before the callback runs.
        *resolver.0.lock().unwrap() = "fresh-token-newtail99".to_string();

        assert_eq!(
            sent_at_build.as_deref(),
            Some("ken-oldtail1"),
            "attribution must describe the bearer the rejected request carried"
        );
        // A record-time re-read (the pre-fix behavior) would report the
        // rotated token instead:
        assert_eq!(
            client.current_sent_bearer_suffix().as_deref(),
            Some("en-newtail99"),
            "sanity: the build-time capture and a live re-read now differ"
        );
    }

    #[test]
    fn live_bearer_resolver_uses_authorization_for_messages_plus_bearer() {
        let cfg = SamplerConfig {
            api_key: Some("stale-bearer".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::Bearer,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-bearer"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/messages");
        let request = builder.build().expect("request should build");
        let auth = request
            .headers()
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok());
        assert_eq!(auth, Some("Bearer fresh-bearer"));
        assert!(request.headers().get("x-api-key").is_none());
    }

    /// Regression: when `api_key` (which seeds `default_headers` with an
    /// `Authorization: Bearer ...`) AND a `bearer_resolver` are both set,
    /// `post()` must produce **exactly one** `Authorization` header on the
    /// wire. The pre-fix code used `RequestBuilder::header(AUTHORIZATION, ...)`
    /// which appends rather than replaces, causing two identical
    /// `Authorization` headers and a 400 from cli-chat-proxy.
    #[test]
    fn post_emits_single_authorization_with_api_key_and_bearer_resolver() {
        let cfg = SamplerConfig {
            api_key: Some("stale-bearer".to_string()),
            api_backend: ApiBackend::Responses,
            auth_scheme: AuthScheme::Bearer,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-bearer"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/responses");
        let request = builder.build().expect("request should build");
        let auth_count = request.headers().get_all(AUTHORIZATION).iter().count();
        assert_eq!(
            auth_count, 1,
            "expected exactly one Authorization header, got {auth_count}"
        );
        assert_eq!(
            request
                .headers()
                .get(AUTHORIZATION)
                .and_then(|v| v.to_str().ok()),
            Some("Bearer fresh-bearer"),
        );
    }

    #[test]
    fn live_bearer_resolver_uses_x_api_key_for_messages_plus_anthropic_api_key() {
        let cfg = SamplerConfig {
            api_key: Some("stale-anthropic".to_string()),
            api_backend: ApiBackend::Messages,
            auth_scheme: AuthScheme::XApiKey,
            bearer_resolver: Some(std::sync::Arc::new(StaticBearerResolver("fresh-anthropic"))),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { builder, .. } = client.post("https://example.test/v1/messages");
        let request = builder.build().expect("request should build");
        let api_key = request
            .headers()
            .get("x-api-key")
            .and_then(|v| v.to_str().ok());
        assert_eq!(api_key, Some("fresh-anthropic"));
        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    /// The callback receives the `post()`-captured fragment only — the
    /// full bearer never crosses the crate boundary.
    #[test]
    fn record_401_attribution_invokes_callback_with_captured_bearer() {
        let cb = std::sync::Arc::new(CountingCallback::default());
        let cb_dyn: crate::attribution::SharedAttributionCallback = cb.clone();
        let cfg = SamplerConfig {
            api_key: Some("the-bearer-1234567890-extra-tail".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            attribution_callback: Some(cb_dyn),
            bearer_resolver: None,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest { sent_bearer, .. } =
            client.post("https://example.test/v1/chat/completions");
        client.record_401_attribution(
            crate::attribution::SamplingConsumer::ChatCompletionsStream,
            sent_bearer.as_deref(),
        );
        let calls = cb.invocations.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].0,
            crate::attribution::SamplingConsumer::ChatCompletionsStream
        );
        assert_eq!(calls[0].1.as_deref(), Some("0-extra-tail"));
        assert_eq!(
            calls[0].1.as_deref().map(str::len),
            Some(crate::attribution::BEARER_SUFFIX_LEN),
        );
    }

    /// When a bearer_resolver is wired but returns `None`, attribution must
    /// report no sent bearer (not the construction-time default header seed).
    #[test]
    fn bearer_resolver_none_attribution_ignores_default_headers() {
        #[derive(Debug)]
        struct EmptyResolver;
        impl crate::config::BearerResolver for EmptyResolver {
            fn current_bearer(&self) -> Option<String> {
                None
            }
        }

        let cfg = SamplerConfig {
            api_key: Some("stale-seed-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(std::sync::Arc::new(EmptyResolver)),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        assert_eq!(
            client.current_sent_bearer_suffix(),
            None,
            "resolver None must not attribute a stripped default seed"
        );
    }

    /// When a bearer_resolver is wired but returns `None` (hard-expired
    /// session with no live AT), default Authorization / x-api-key must be
    /// stripped so a stale seed key cannot ride the wire.
    #[test]
    fn bearer_resolver_none_strips_default_authorization() {
        #[derive(Debug)]
        struct EmptyResolver;
        impl crate::config::BearerResolver for EmptyResolver {
            fn current_bearer(&self) -> Option<String> {
                None
            }
        }

        let cfg = SamplerConfig {
            api_key: Some("stale-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(std::sync::Arc::new(EmptyResolver)),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        let SentRequest {
            builder,
            sent_bearer: sent,
        } = client.post("https://example.test/v1/responses");
        let request = builder.body("").build().expect("request should build");
        assert_eq!(sent, None, "capture must agree: nothing was sent");
        assert!(
            request.headers().get(AUTHORIZATION).is_none(),
            "stale default Authorization must not be sent when resolver is empty"
        );
    }

    /// Regression test: when a bearer_resolver is wired, `post()` must
    /// *replace* the Authorization header from `default_headers`, not
    /// append a second one. Duplicate Authorization headers cause
    /// Cloudflare to return 400 Bad Request.
    #[test]
    fn bearer_resolver_replaces_authorization_header() {
        #[derive(Debug)]
        struct StaticResolver(String);
        impl crate::config::BearerResolver for StaticResolver {
            fn current_bearer(&self) -> Option<String> {
                Some(self.0.clone())
            }
        }

        let resolver: crate::config::SharedBearerResolver =
            std::sync::Arc::new(StaticResolver("fresh-token".to_string()));
        let cfg = SamplerConfig {
            api_key: Some("stale-token".to_string()),
            api_backend: ApiBackend::Responses,
            bearer_resolver: Some(resolver),
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");

        // Build a request to inspect the final headers.
        let SentRequest { builder, .. } = client.post("https://example.test/v1/responses");
        let request = builder.body("").build().expect("request should build");

        let auth_values: Vec<_> = request.headers().get_all(AUTHORIZATION).iter().collect();
        assert_eq!(
            auth_values.len(),
            1,
            "expected exactly one Authorization header, got {}: {:?}",
            auth_values.len(),
            auth_values
        );
        assert_eq!(
            auth_values[0].to_str().unwrap(),
            "Bearer fresh-token",
            "Authorization header should contain the resolver's fresh token"
        );
    }

    /// `record_401_attribution` is a no-op when `attribution_callback`
    /// is `None` (the BYOK / sampler-only path). The previous tests
    /// in this module construct clients without a callback and rely
    /// on this property holding.
    #[test]
    fn record_401_attribution_is_noop_without_callback() {
        let cfg = SamplerConfig {
            api_key: Some("bearer".to_string()),
            api_backend: ApiBackend::ChatCompletions,
            attribution_callback: None,
            bearer_resolver: None,
            ..minimal_config()
        };
        let client = SamplingClient::new(cfg).expect("client should build");
        // Must not panic.
        client.record_401_attribution(
            crate::attribution::SamplingConsumer::ChatCompletions,
            Some("bearer-tail-12"),
        );
    }

    /// `response.completed` carrying
    /// `usage.context_details.{input_tokens, output_tokens}` rewrites
    /// `usage.total_tokens` in place to the live context length
    /// (`ctx.input + ctx.output`). Billing fields stay on the wire's
    /// cumulative values.
    #[test]
    fn deserialize_response_event_overrides_total_tokens_from_context_details() {
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "echo-agent-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 6003,
                    "input_tokens_details": { "cached_tokens": 1984 },
                    "output_tokens": 711,
                    "output_tokens_details": { "reasoning_tokens": 388 },
                    "total_tokens": 6714,
                    "context_details": {
                        "input_tokens": 5022,
                        "output_tokens": 571
                    }
                }
            }
        }"#;
        let event = deserialize_response_event(sse).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        // Billing fields stay cumulative — unchanged by context_details.
        assert_eq!(usage.input_tokens, 6003);
        assert_eq!(usage.output_tokens, 711);
        assert_eq!(usage.input_tokens_details.cached_tokens, 1984);
        assert_eq!(usage.output_tokens_details.reasoning_tokens, 388);
        // total_tokens rewritten to ctx.input + ctx.output (5022 + 571).
        // NOT the wire's cumulative total (6714).
        assert_eq!(usage.total_tokens, 5_593);
    }

    #[test]
    fn deserialize_response_event_stashes_cost_in_metadata() {
        let make = |ticks: i64| {
            format!(
                r#"{{
                "type": "response.completed",
                "sequence_number": 0,
                "response": {{
                    "id": "resp_1", "object": "response", "created_at": 0,
                    "model": "echo-agent-build", "status": "completed", "output": [],
                    "usage": {{
                        "input_tokens": 10,
                        "input_tokens_details": {{ "cached_tokens": 0 }},
                        "output_tokens": 5,
                        "output_tokens_details": {{ "reasoning_tokens": 0 }},
                        "total_tokens": 15,
                        "cost_in_usd_ticks": {ticks}
                    }}
                }}
            }}"#
            )
        };

        let event = deserialize_response_event(&make(78)).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        assert_eq!(
            e.response
                .metadata
                .as_ref()
                .and_then(|m| m.get(COST_USD_TICKS_METADATA_KEY))
                .map(String::as_str),
            Some("78")
        );

        // The REST mapper backfills 0 for unbilled requests: no stash.
        let event = deserialize_response_event(&make(0)).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        assert!(e.response.metadata.is_none());
    }

    #[test]
    fn deserialize_response_event_total_tokens_unchanged_when_context_details_absent() {
        // Older / non-Responses backends omit `context_details`.
        // `total_tokens` passes through from the wire unchanged.
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "echo-agent-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 10000,
                    "input_tokens_details": { "cached_tokens": 0 },
                    "output_tokens": 100,
                    "output_tokens_details": { "reasoning_tokens": 0 },
                    "total_tokens": 10100
                }
            }
        }"#;
        let event = deserialize_response_event(sse).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        assert_eq!(usage.total_tokens, 10_100);
    }

    #[test]
    fn deserialize_response_event_total_tokens_unchanged_when_context_details_partial() {
        // Defensive: if the backend ever ships only one of the two
        // context_details fields, we don't have a complete picture of
        // the live context size, so leave `total_tokens` on the wire's
        // cumulative value instead of guessing (treating the missing
        // half as 0 would silently under-report).
        let sse = r#"{
            "type": "response.completed",
            "sequence_number": 0,
            "response": {
                "id": "resp_1",
                "object": "response",
                "created_at": 0,
                "model": "echo-agent-build",
                "status": "completed",
                "output": [],
                "usage": {
                    "input_tokens": 6003,
                    "input_tokens_details": { "cached_tokens": 1984 },
                    "output_tokens": 711,
                    "output_tokens_details": { "reasoning_tokens": 388 },
                    "total_tokens": 6714,
                    "context_details": {
                        "input_tokens": 5022
                    }
                }
            }
        }"#;
        let event = deserialize_response_event(sse).expect("parse");
        let rs::ResponseStreamEvent::ResponseCompleted(e) = event else {
            panic!("expected ResponseCompleted");
        };
        let usage = e.response.usage.expect("usage present");
        assert_eq!(usage.total_tokens, 6_714);
    }

    #[test]
    fn deserialize_response_event_ignores_context_details_on_non_terminal_events() {
        // Non-terminal events don't carry final usage; even if the backend ever
        // echoed `context_details` on one, we don't touch it.
        let sse = r#"{
            "type": "response.output_text.delta",
            "sequence_number": 0,
            "item_id": "item-1",
            "output_index": 0,
            "content_index": 0,
            "delta": "hello",
            "logprobs": []
        }"#;
        let event = deserialize_response_event(sse).expect("non-terminal event parses");
        assert!(matches!(
            event,
            rs::ResponseStreamEvent::ResponseOutputTextDelta(_)
        ));
    }
}
