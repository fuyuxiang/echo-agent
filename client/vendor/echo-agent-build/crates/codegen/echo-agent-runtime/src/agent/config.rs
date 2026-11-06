use crate::agent::auth_method::ModelByok;
use crate::agent::model_providers::{
    ModelProviderConfig, auth_config_issues, model_provider_auth_name, parse_model_providers,
};
use crate::auth::{AuthManager, EchoAgentComConfig, OidcAuthConfig};
use crate::remote::DEFAULT_CONTEXT_WINDOW;
use crate::{config::StorageMode, sampling::ApiBackend, tools::config::ShellToolsetConfig};
use agent_client_protocol as acp;
use echo_agent_core::prompt::skills::SkillsConfig;
use echo_agent_sampler::{AuthScheme, SamplerConfig};
use echo_agent_sampling_types::{
    CompactionAtTokens, CompactionsRemaining, REASONING_EFFORT_META_KEY,
    REASONING_EFFORTS_META_KEY, ReasoningEffort, ReasoningEffortOption,
    reasoning_effort_meta_value, reasoning_efforts_meta_value,
};
use echo_agent_tools::types::compat::{
    COMPAT_CELLS, CompatConfig, CompatConfigToml, CompatRemoteKey, CompatSurface, CompatVendor,
};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::Arc;
/// The mode in which the agent is running.
/// Determines behavior like relay sync enablement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentMode {
    /// TUI interactive mode - full UI with relay sync support
    Tui,
    /// Headless mode - no UI, connected to relay WebSocket
    Headless,
    /// Stdio mode - JSON-RPC over stdin/stdout
    Stdio,
    /// Server mode - WebSocket server for external clients
    Serve,
    /// Leader mode - IPC server for follower clients
    Leader,
    /// Generic/unknown mode
    #[default]
    Generic,
}
/// Default agent type when the server or user config doesn't specify one.
pub const DEFAULT_AGENT_TYPE: &str = "echo-agent-build-plan";
/// Serde default for `ModelInfo.agent_type` and `ModelEntryConfig.agent_type`.
pub(crate) fn default_agent_type() -> String {
    DEFAULT_AGENT_TYPE.to_owned()
}
/// Default base URL for the cli chat proxy.
pub const CLI_CHAT_PROXY_BASE_URL_DEFAULT: &str = "https://chat-proxy.echo-agent.invalid/v1";
/// Default base URL for the public EchoAgent API.
pub const ECHO_AGENT_API_BASE_URL_DEFAULT: &str = "https://api.echo-agent.invalid/v1";
const NO_INLINE_CITATIONS_RESPONSE_INCLUDE: &str = "no_inline_citations";
/// One or more environment variable names that may hold a model API key.
///
/// Serde `untagged`: accepts a string or an array in TOML/JSON.
///
/// ```toml
/// env_key = "ANTHROPIC_AUTH_TOKEN"
/// # or
/// env_key = ["ANTHROPIC_AUTH_TOKEN", "LC_ANTHROPIC_AUTH_TOKEN"]
/// ```
///
/// At resolve time the **first set, non-blank** value wins (e.g. SSH
/// `AcceptEnv LC_*` forwarding of the Bottlerocket token).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EnvKeys {
    One(String),
    Many(Vec<String>),
}
impl EnvKeys {
    /// Single-name convenience constructor.
    pub fn single(name: impl Into<String>) -> Self {
        Self::One(name.into())
    }
    /// Construct from an ordered list (empty names dropped; 0/1/N → Many/One/Many).
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let names: Vec<String> = names
            .into_iter()
            .map(Into::into)
            .filter(|s| !s.is_empty())
            .collect();
        match names.as_slice() {
            [] => Self::Many(Vec::new()),
            [_] => Self::One(names.into_iter().next().expect("len 1")),
            _ => Self::Many(names),
        }
    }
    pub fn is_empty(&self) -> bool {
        match self {
            Self::One(s) => s.is_empty(),
            Self::Many(v) => v.is_empty(),
        }
    }
    /// Configured names in priority order.
    pub fn names(&self) -> Vec<&str> {
        match self {
            Self::One(s) => vec![s.as_str()],
            Self::Many(v) => v.iter().map(String::as_str).collect(),
        }
    }
    /// First name only (useful for single-key assertions / display).
    pub fn primary(&self) -> Option<&str> {
        match self {
            Self::One(s) if !s.is_empty() => Some(s.as_str()),
            Self::One(_) => None,
            Self::Many(v) => v.iter().map(String::as_str).find(|s| !s.is_empty()),
        }
    }
    /// Resolve the first set, non-blank process env value among configured names.
    pub(crate) fn resolve_value(&self) -> Option<String> {
        self.resolve_value_with(|name| std::env::var(name).ok())
    }
    /// Testable resolve with an injected getenv.
    pub(crate) fn resolve_value_with(
        &self,
        mut getenv: impl FnMut(&str) -> Option<String>,
    ) -> Option<String> {
        for name in self.names() {
            if let Some(value) = getenv(name)
                && !value.trim().is_empty()
            {
                return Some(value);
            }
        }
        None
    }
}
/// Semantic equality: compares the ordered name lists, so `One("X")` and
/// `Many(["X"])` (the shape serde produces for `["X"]`) compare equal.
impl PartialEq for EnvKeys {
    fn eq(&self, other: &Self) -> bool {
        self.names() == other.names()
    }
}
impl Eq for EnvKeys {}
impl std::fmt::Display for EnvKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.names().join(", "))
    }
}
/// Configuration for API endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct EndpointsConfig {
    /// cli chat proxy base URL. `None` = unset (resolvers apply the default);
    /// `Some` = explicitly configured. Tracking explicitness (vs comparing to the
    /// default value) lets an org pin the proxy to the default on purpose.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli_chat_proxy_base_url: Option<String>,
    /// Base URL for the public EchoAgent API.
    pub echo_agent_api_base_url: String,
    /// Optional extra access-header value (applied only with the optional
    /// non-production feature, and only for matching first-party hosts).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alpha_test_key: Option<String>,
    /// Env: `ECHO_AGENT_MODELS_BASE_URL`. Enables custom endpoint mode.
    /// List URL defaults to `{models_base_url}/models`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_base_url: Option<String>,
    /// Env: `ECHO_AGENT_MODELS_LIST_URL`. Overrides the default `{base}/models` list URL.
    #[serde(alias = "models_endpoint", skip_serializing_if = "Option::is_none")]
    pub models_list_url: Option<String>,
    /// Env: `ECHO_AGENT_FEEDBACK_BASE_URL`. Where feedback submissions go.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback_base_url: Option<String>,
    /// Env: `ECHO_AGENT_TRACE_UPLOAD_URL`. Where trace uploads go.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_upload_url: Option<String>,
    /// Env: `ECHO_AGENT_TRACE_UPLOAD_BUCKET`. Direct bucket (`gs://` or `s3://`), bypasses proxy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_upload_bucket: Option<String>,
    /// Env: `ECHO_AGENT_TRACE_UPLOAD_REGION`. AWS region (S3 only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_upload_region: Option<String>,
    /// Env: `ECHO_AGENT_TRACE_UPLOAD_CREDENTIALS_FILE`. Path to GCS SA key or AWS credentials file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_upload_credentials_file: Option<String>,
    /// Inline credentials (JSON/INI). Takes precedence over `credentials_file`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_upload_credentials: Option<String>,
    /// Env: `ECHO_AGENT_TRACE_UPLOAD_ENDPOINT_URL`. Custom S3-compatible endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_upload_endpoint_url: Option<String>,
    /// Env: `ECHO_AGENT_DEPLOYMENT_KEY`. Management API key for enterprise deployments.
    /// Sent on telemetry and service requests for deployment-level attribution.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment_key: Option<String>,
    /// Env: `ECHO_AGENT_MANAGED_CONFIG_URL`. Override the managed config endpoint.
    /// Defaults to `{proxy_url()}/deployment/config`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub managed_config_url: Option<String>,
    /// Env: `OTEL_EXPORTER_OTLP_ENDPOINT`. OTLP collector base; `/v1/traces` is
    /// appended. Legacy repoint of the INTERNAL trace pipeline — deprecated in
    /// favor of `ECHO_AGENT_INTERNAL_OTLP_TRACES_ENDPOINT`, and ignored by the internal
    /// pipeline when `ECHO_AGENT_EXTERNAL_OTEL` is set (the standard `OTEL_*` vars then
    /// route the external stream only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otel_exporter_otlp_endpoint: Option<String>,
    /// Env: `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`. Full traces endpoint, used
    /// verbatim; overrides `otel_exporter_otlp_endpoint`. Same legacy/deprecation
    /// semantics as `otel_exporter_otlp_endpoint`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otel_exporter_otlp_traces_endpoint: Option<String>,
    /// Env: `OTEL_EXPORTER_OTLP_HEADERS`. `k=v,k2=v2`; merged onto export headers.
    /// Same legacy/deprecation semantics as `otel_exporter_otlp_endpoint`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otel_exporter_otlp_headers: Option<String>,
    /// Env: `ECHO_AGENT_INTERNAL_OTLP_TRACES_ENDPOINT`. Full INTERNAL traces endpoint,
    /// used verbatim. Dev/debug repoint of the internal span firehose (replaces
    /// the legacy `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` behavior; used by
    /// local-ic-testing / internal dev flows). Wins over the legacy `OTEL_*` vars.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub echo_agent_internal_otlp_traces_endpoint: Option<String>,
    /// Env: `ECHO_AGENT_INTERNAL_OTLP_HEADERS`. `k=v,k2=v2` extra headers for the
    /// internal export (debug). Wins over the legacy `OTEL_EXPORTER_OTLP_HEADERS`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub echo_agent_internal_otlp_headers: Option<String>,
    /// External-OTEL master switch, captured at construction via
    /// [`external_otel_master_switch_resolved`] — the same layered resolution
    /// (requirement pin > `ECHO_AGENT_EXTERNAL_OTEL` env > `[telemetry].otel_enabled`
    /// config, managed layers included) that activates the external stream.
    /// When set, the standard `OTEL_EXPORTER_OTLP_*` vars are reserved for the
    /// external OTEL stream and the internal trace pipeline ignores them
    /// entirely — an admin who opts in (by *any* layer, including an org
    /// enable distributed via managed config with no env var) never receives
    /// the internally-authed firehose. Held as a field (not re-read in the
    /// resolvers) so the resolvers stay pure and testable without env races.
    #[serde(skip)]
    pub external_otel_master_switch: bool,
    /// Env: `OTEL_TRACES_EXPORTER`. `otlp` (default) or `none` to disable spans.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otel_traces_exporter: Option<String>,
    /// Env: `OTEL_BSP_SCHEDULE_DELAY` (OTel) or `OTEL_TRACES_EXPORT_INTERVAL`
    /// (Claude alias). Batch flush interval (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otel_traces_export_interval: Option<u64>,
    /// Env: `OTEL_EXPORTER_OTLP_TIMEOUT`. Export HTTP timeout (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub otel_exporter_otlp_timeout: Option<u64>,
    /// Read by `load_management_api_key_sync()`. Declared for `serde_ignored`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_api_key: Option<String>,
    /// Read by `load_gcs_service_account_key_sync()`. Declared for `serde_ignored`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gcs_service_account_key: Option<String>,
}
/// A blank or whitespace-only override counts as unset. Single source of truth
/// for the "empty value = not configured" rule shared by the endpoint resolvers.
fn blank_as_unset(opt: &Option<String>) -> Option<String> {
    opt.as_deref()
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
}
/// Parse a `k=v,k2=v2` OTLP header list (the `OTEL_EXPORTER_OTLP_HEADERS`
/// format, shared with `ECHO_AGENT_INTERNAL_OTLP_HEADERS`): split on `,`,
/// `split_once('=')`, trim key/value, skip blank keys, keep empty values.
fn parse_otlp_header_list(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}
impl EndpointsConfig {
    pub fn has_custom_endpoint(&self) -> bool {
        self.models_base_url.is_some() || self.models_list_url.is_some()
    }
    /// `default()` plus merged managed/requirements endpoint overrides, so
    /// startup fetches use the configured (not public) endpoints. Only merges
    /// layers — never derives one endpoint from another. Falls back to
    /// `default()` on load failure.
    pub(crate) fn from_effective_config() -> Self {
        match crate::config::load_effective_config() {
            Ok(cfg) => Self::from_config_value(&cfg),
            Err(_) => Self::default(),
        }
    }
    /// Layer the `[endpoints]` table from `config` over the env/default base.
    /// No field is derived from another — defaulting is done by the resolvers.
    /// `pub`: the pager resolves the voice STT base through this same path.
    pub fn from_config_value(config: &toml::Value) -> Self {
        let default = Self::default();
        let external_otel_master_switch = default.external_otel_master_switch;
        let mut base = match toml::Value::try_from(default) {
            Ok(v) => v,
            Err(_) => return Self::default(),
        };
        if let Some(endpoints) = config.get("endpoints") {
            crate::config::deep_merge_toml(&mut base, endpoints);
        }
        let mut resolved: Self = base.try_into().unwrap_or_default();
        resolved.external_otel_master_switch = external_otel_master_switch;
        resolved
    }
    /// The cli-chat-proxy base URL through which all auxiliary services (and
    /// OAuth/session inference) resolve: explicit `cli_chat_proxy_base_url`, else
    /// the public default. NEVER falls back to `echo_agent_api_base_url` — that is the
    /// inference endpoint (API-key auth) only.
    pub fn proxy_url(&self) -> String {
        blank_as_unset(&self.cli_chat_proxy_base_url)
            .unwrap_or_else(|| CLI_CHAT_PROXY_BASE_URL_DEFAULT.to_owned())
    }
    pub(crate) fn resolve_inference_base_url(&self) -> String {
        self.models_base_url
            .clone()
            .unwrap_or_else(|| self.proxy_url())
    }
    /// Feedback endpoint — an auxiliary service, so it defaults to the
    /// cli-chat-proxy, never `echo_agent_api_base_url`.
    pub(crate) fn resolve_feedback_base_url(&self) -> String {
        blank_as_unset(&self.feedback_base_url).unwrap_or_else(|| self.proxy_url())
    }
    /// Trace upload endpoint — an auxiliary service, so it defaults to the
    /// cli-chat-proxy, never `echo_agent_api_base_url`.
    pub(crate) fn resolve_trace_upload_url(&self) -> String {
        blank_as_unset(&self.trace_upload_url).unwrap_or_else(|| self.proxy_url())
    }
    /// Managed deployment-config URL (`echoagent setup`): explicit `managed_config_url`,
    /// else `proxy_url` + `/deployment/config`. Never `echo_agent_api_base_url`, so the
    /// deployment key reaches the proxy, not the inference host.
    pub(crate) fn resolve_managed_config_url(&self) -> String {
        blank_as_unset(&self.managed_config_url).unwrap_or_else(|| {
            format!(
                "{}/deployment/config",
                self.proxy_url().trim_end_matches('/')
            )
        })
    }
    /// INTERNAL OTLP traces endpoint. Precedence:
    /// 1. `echo_agent_internal_otlp_traces_endpoint` (verbatim)
    /// 2. legacy `otel_exporter_otlp_traces_endpoint` (verbatim) >
    ///    `otel_exporter_otlp_endpoint` + `/v1/traces` — ONLY when the
    ///    external-OTEL master switch is unset (back-compat; deprecated)
    /// 3. `proxy_url` + `/traces`.
    /// Uses the proxy default (not the `echo_agent_api_base_url` fallback) so
    /// telemetry reports to EchoAgent even when inference is overridden. When the
    /// master switch IS set, the standard `OTEL_EXPORTER_OTLP_*` values are
    /// completely ignored here so the internally-authed firehose never lands
    /// at an external collector.
    pub(crate) fn resolve_otlp_traces_endpoint(&self) -> String {
        if let Some(full) = blank_as_unset(&self.echo_agent_internal_otlp_traces_endpoint) {
            return full.trim_end_matches('/').to_string();
        }
        if !self.external_otel_master_switch
            && let Some(legacy) = self.legacy_internal_otlp_traces_endpoint()
        {
            tracing::warn!(
                "Repointing the internal trace pipeline via OTEL_EXPORTER_OTLP_ENDPOINT / \
                 OTEL_EXPORTER_OTLP_TRACES_ENDPOINT is deprecated; use \
                 ECHO_AGENT_INTERNAL_OTLP_TRACES_ENDPOINT instead — the standard OTEL_* vars will \
                 route the external OTEL stream only in a future release"
            );
            return legacy;
        }
        format!("{}/traces", self.proxy_url().trim_end_matches('/'))
    }
    /// Legacy (standard-OTEL-var) internal traces endpoint, if any:
    /// `otel_exporter_otlp_traces_endpoint` verbatim, else
    /// `otel_exporter_otlp_endpoint` + `/v1/traces`. Ignores the master switch.
    fn legacy_internal_otlp_traces_endpoint(&self) -> Option<String> {
        if let Some(full) = blank_as_unset(&self.otel_exporter_otlp_traces_endpoint) {
            return Some(full.trim_end_matches('/').to_string());
        }
        blank_as_unset(&self.otel_exporter_otlp_endpoint)
            .map(|base| format!("{}/v1/traces", base.trim_end_matches('/')))
    }
    /// Extra headers for the INTERNAL export: `echo_agent_internal_otlp_headers`
    /// first; legacy fallback to `otel_exporter_otlp_headers` ONLY when the
    /// external-OTEL master switch is unset (back-compat for existing users).
    pub(crate) fn resolve_otlp_headers(&self) -> Vec<(String, String)> {
        if let Some(headers) = blank_as_unset(&self.echo_agent_internal_otlp_headers) {
            return parse_otlp_header_list(&headers);
        }
        if !self.external_otel_master_switch {
            return parse_otlp_header_list(
                self.otel_exporter_otlp_headers.as_deref().unwrap_or(""),
            );
        }
        Vec::new()
    }
    /// Whether the legacy fallback actually supplied the internal endpoint OR
    /// internal headers from the standard `OTEL_EXPORTER_OTLP_*` vars — i.e.
    /// the master switch is unset AND (`otel_exporter_otlp_traces_endpoint` /
    /// `otel_exporter_otlp_endpoint` is non-blank for the endpoint, or
    /// `otel_exporter_otlp_headers` is non-blank for headers) AND no
    /// `echo_agent_internal_otlp_*` override shadowed that half.
    ///
    /// CONTRACT: this flag is passed to the external OTEL stream's init, which
    /// MUST refuse to activate when it is true — the same standard vars cannot
    /// feed both pipelines (no-double-send invariant, enforced in code).
    pub(crate) fn internal_otlp_consumed_standard_vars(&self) -> bool {
        if self.external_otel_master_switch {
            return false;
        }
        let endpoint_consumed = blank_as_unset(&self.echo_agent_internal_otlp_traces_endpoint)
            .is_none()
            && self.legacy_internal_otlp_traces_endpoint().is_some();
        let headers_consumed = blank_as_unset(&self.echo_agent_internal_otlp_headers).is_none()
            && blank_as_unset(&self.otel_exporter_otlp_headers).is_some();
        endpoint_consumed || headers_consumed
    }
    /// Trace export enabled unless `OTEL_TRACES_EXPORTER=none`. Deliberately
    /// still honored by the internal pipeline even with `ECHO_AGENT_EXTERNAL_OTEL`
    /// set: disabling internal span export is the safe direction.
    pub(crate) fn resolve_traces_export_enabled(&self) -> bool {
        !matches!(
            self.otel_traces_exporter.as_deref().map(str::trim),
            Some("none")
        )
    }
    /// `OTEL_BSP_SCHEDULE_DELAY` / `OTEL_TRACES_EXPORT_INTERVAL` — tuning-only,
    /// deliberately shared between the internal and external pipelines.
    pub(crate) fn resolve_otlp_export_interval(&self) -> Option<std::time::Duration> {
        self.otel_traces_export_interval
            .map(std::time::Duration::from_millis)
    }
    /// `OTEL_EXPORTER_OTLP_TIMEOUT` — tuning-only, deliberately shared between
    /// the internal and external pipelines.
    pub(crate) fn resolve_otlp_timeout(&self) -> Option<std::time::Duration> {
        self.otel_exporter_otlp_timeout
            .map(std::time::Duration::from_millis)
    }
    /// Resolve trace upload credentials: inline > file > `None` (ambient).
    pub(crate) fn resolve_trace_credentials(&self) -> Option<String> {
        if let Some(ref inline) = self.trace_upload_credentials {
            let trimmed = inline.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
        self.trace_upload_credentials_file
            .as_deref()
            .and_then(|path| {
                std::fs::read_to_string(path)
                    .inspect_err(|e| {
                        tracing::warn!(
                            path = %path,
                            error = %e,
                            "Failed to read trace upload credentials file"
                        );
                    })
                    .ok()
            })
    }
    /// Resolve direct-to-bucket upload method from `trace_upload_bucket`.
    /// Returns `None` if no bucket is configured or scheme is unrecognized.
    pub fn resolve_direct_upload_method(
        &self,
    ) -> Option<crate::session::repo_changes::UploadMethod> {
        let bucket_url = self.trace_upload_bucket.as_deref()?.trim();
        if bucket_url.is_empty() {
            return None;
        }
        if let Some(bucket_name) = bucket_url
            .strip_prefix("s3://")
            .map(|s| s.trim_end_matches('/'))
        {
            let region = self
                .trace_upload_region
                .clone()
                .unwrap_or_else(|| "us-east-1".to_owned());
            return Some(crate::session::repo_changes::UploadMethod::S3 {
                bucket: bucket_name.to_owned(),
                region,
                credentials_file: None,
                credentials_content: self.resolve_trace_credentials(),
                endpoint_url: self.trace_upload_endpoint_url.clone(),
            });
        }
        if bucket_url.starts_with("gs://") {
            return Some(crate::session::repo_changes::UploadMethod::Direct {
                service_account_key: self.resolve_trace_credentials(),
            });
        }
        tracing::warn!(
            bucket = %bucket_url,
            "trace_upload_bucket has unrecognized scheme (expected gs:// or s3://), ignoring"
        );
        None
    }
    /// Whether trace upload can authenticate without an interactive login.
    pub fn has_noninteractive_upload_auth(&self) -> bool {
        self.deployment_key.is_some() || self.resolve_direct_upload_method().is_some()
    }
    /// Direct bucket → proxy (if `auth_token` or `deployment_key`) → ambient GCS → `None`.
    pub fn resolve_upload_method(
        &self,
        auth_token: Option<String>,
    ) -> Option<crate::session::repo_changes::UploadMethod> {
        if let Some(method) = self.resolve_direct_upload_method() {
            return Some(method);
        }
        if auth_token.is_some() || self.deployment_key.is_some() {
            return Some(crate::session::repo_changes::UploadMethod::Proxy {
                proxy_base_url: self.resolve_trace_upload_url(),
                user_token: auth_token.unwrap_or_default(),
                deployment_key: self.deployment_key.clone(),
                alpha_test_key: self.alpha_test_key.clone(),
            });
        }
        let service_account_key = crate::util::config::load_gcs_service_account_key_sync();
        if service_account_key.is_some() {
            return Some(crate::session::repo_changes::UploadMethod::Direct {
                service_account_key,
            });
        }
        None
    }
    /// Resolve trace bucket URL: env > config > compiled-in default.
    /// `None` disables direct GCS trace uploads.
    pub fn resolve_trace_bucket_url(&self) -> Option<Resolved<String>> {
        resolve_string_flag(
            None,
            "ECHO_AGENT_TELEMETRY_GCS_BUCKET",
            self.trace_upload_bucket.as_deref(),
            None,
        )
        .or_else(|| {
            crate::upload::gcs::SESSION_TRACES_BUCKET
                .map(|b| Resolved::new(format!("gs://{b}"), ConfigSource::Default))
        })
    }
    /// `models_list_url` > `{models_base_url}/models` > `{proxy_base_url}/models`.
    pub(crate) fn resolve_models_list_url(&self) -> String {
        if let Some(ref url) = self.models_list_url {
            return url.clone();
        }
        let base = self
            .models_base_url
            .clone()
            .unwrap_or_else(|| self.proxy_url());
        format!("{}/models", base)
    }
}
impl Default for EndpointsConfig {
    fn default() -> Self {
        Self {
            cli_chat_proxy_base_url: std::env::var("ECHO_AGENT_CLI_CHAT_PROXY_BASE_URL").ok(),
            echo_agent_api_base_url: std::env::var("ECHO_AGENT_API_BASE_URL")
                .unwrap_or_else(|_| ECHO_AGENT_API_BASE_URL_DEFAULT.to_owned()),
            alpha_test_key: None,
            models_base_url: env_string("ECHO_AGENT_MODELS_BASE_URL"),
            models_list_url: env_string("ECHO_AGENT_MODELS_LIST_URL"),
            feedback_base_url: env_string("ECHO_AGENT_FEEDBACK_BASE_URL"),
            trace_upload_url: env_string("ECHO_AGENT_TRACE_UPLOAD_URL"),
            trace_upload_bucket: env_string("ECHO_AGENT_TRACE_UPLOAD_BUCKET"),
            trace_upload_region: env_string("ECHO_AGENT_TRACE_UPLOAD_REGION"),
            trace_upload_credentials_file: env_string("ECHO_AGENT_TRACE_UPLOAD_CREDENTIALS_FILE"),
            trace_upload_credentials: None,
            trace_upload_endpoint_url: env_string("ECHO_AGENT_TRACE_UPLOAD_ENDPOINT_URL"),
            deployment_key: env_string("ECHO_AGENT_DEPLOYMENT_KEY"),
            managed_config_url: env_string("ECHO_AGENT_MANAGED_CONFIG_URL"),
            otel_exporter_otlp_endpoint: env_string("OTEL_EXPORTER_OTLP_ENDPOINT"),
            otel_exporter_otlp_traces_endpoint: env_string("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT"),
            otel_exporter_otlp_headers: env_string("OTEL_EXPORTER_OTLP_HEADERS"),
            echo_agent_internal_otlp_traces_endpoint: env_string(
                "ECHO_AGENT_INTERNAL_OTLP_TRACES_ENDPOINT",
            ),
            echo_agent_internal_otlp_headers: env_string("ECHO_AGENT_INTERNAL_OTLP_HEADERS"),
            external_otel_master_switch: external_otel_master_switch_resolved(),
            otel_traces_exporter: env_string("OTEL_TRACES_EXPORTER"),
            otel_traces_export_interval: env_string("OTEL_BSP_SCHEDULE_DELAY")
                .or_else(|| env_string("OTEL_TRACES_EXPORT_INTERVAL"))
                .and_then(|s| s.parse().ok()),
            otel_exporter_otlp_timeout: env_string("OTEL_EXPORTER_OTLP_TIMEOUT")
                .and_then(|s| s.parse().ok()),
            management_api_key: None,
            gcs_service_account_key: None,
        }
    }
}
pub use echo_agent_config_types::{
    BoolFlag, ConfigSource, FEATURES, Feature, FeatureSources, LazinessDetectorPerModelConfig,
    Resolved,
};
/// Resolution result for a `/goal` role's model selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum GoalRoleModelChoice {
    /// Use the current (parent) model + the parent's agent type.
    #[default]
    InheritCurrent,
    /// Use this explicit pair (subject to auth/fail-open at spawn time).
    Explicit(crate::util::config::GoalRoleModel),
}
/// A requirement pin from `requirements.toml`. Wins over all other sources.
#[derive(Debug, Clone, Default)]
pub struct Constrained<T> {
    pin: Option<T>,
    source: Option<crate::config::RequirementSource>,
}
impl<T: Clone> Constrained<T> {
    pub fn pin(&mut self, value: T, source: crate::config::RequirementSource) {
        self.pin = Some(value);
        self.source = Some(source);
    }
    pub fn pinned(&self) -> Option<T> {
        self.pin.clone()
    }
    pub fn source(&self) -> Option<&crate::config::RequirementSource> {
        self.source.as_ref()
    }
}
/// Enforced requirements from `requirements.toml`. Pinned values win over all other sources.
#[derive(Debug, Clone, Default)]
pub struct Requirements {
    pub telemetry: Constrained<TelemetryMode>,
    pub trace_upload: Constrained<bool>,
    pub image_gen: Constrained<bool>,
    pub image_edit: Constrained<bool>,
    pub video_gen: Constrained<bool>,
    pub sandbox_auto_allow_bash: Constrained<bool>,
    pub sandbox_profile: Constrained<String>,
    pub respect_gitignore: Constrained<bool>,
    pub remote_fetch: Constrained<bool>,
    pub title_refresh: Constrained<bool>,
    /// Pins from a requirements layer or an MDM policy, keyed by [`Feature`].
    features: BTreeMap<Feature, Constrained<bool>>,
}
impl Requirements {
    pub(crate) fn pin_feature(
        &mut self,
        feature: Feature,
        value: bool,
        source: crate::config::RequirementSource,
    ) {
        self.features.entry(feature).or_default().pin(value, source);
    }
    pub(crate) fn pinned_feature(&self, feature: Feature) -> Option<bool> {
        self.features.get(&feature).and_then(Constrained::pinned)
    }
}
/// Inputs for resolving `#[serde(skip)]` runtime fields after `new_from_toml_cfg()`.
///
/// Constructed by each binary from its CLI args and startup state, then passed
/// to [`Config::resolve_runtime_fields`].
pub struct RuntimeResolutionContext<'a> {
    pub raw_config: &'a toml::Value,
    pub remote_settings: Option<&'a crate::util::config::RemoteSettings>,
    pub is_headless: bool,
    /// `Some(true)` = CLI explicitly enabled, `None` = defer to config/env/remote.
    pub cli_subagents: Option<bool>,
    pub cli_web_search_model: Option<&'a str>,
    pub cli_session_summary_model: Option<&'a str>,
    /// CLI memory override set by a legacy compatibility flag.
    pub memory_enabled_override: Option<bool>,
    /// CLI `--disable-web-search` flag. ORed with config.toml value.
    pub disable_web_search: bool,
    /// CLI `--todo-gate` flag. Session-scoped — not persisted.
    pub todo_gate: bool,
    /// CLI `--laziness-debug-log <path>`. When `Some`, the Layer-3
    /// classifier fires after every turn (bypassing the idle wait /
    /// per-model gate / nudge cap) and writes a JSONL line per fire.
    /// Observation-only. Session-scoped — not persisted.
    pub laziness_debug_log: Option<&'a std::path::Path>,
    /// CLI `--storage-mode` override. `None` = defer to env/remote/default.
    pub storage_mode: Option<&'a str>,
}
/// First-party credential env vars scrubbed from a BYOK auth-provider helper's
/// environment so it can't inherit the keys EchoAgent uses for its own first-party
/// requests. Keep in sync with every first-party credential env read across the
/// crate: `auth::manager` (`ECHO_AGENT_AUTH`/`ECHO_AGENT_AUTH_PATH`), `auth_method`
/// (`ECHO_AGENT_API_KEY`/legacy), and the credential-bearing `env_string(...)` reads in
/// `EndpointsConfig::default`. The `provider_helper_env_scrubs_first_party_credentials`
/// test pins this against an independent audited literal, so any change here must
/// be mirrored (and re-audited) there.
pub(crate) const FIRST_PARTY_CREDENTIAL_ENV_VARS: &[&str] = &[
    crate::agent::auth_method::ECHO_AGENT_API_KEY_ENV_VAR,
    crate::agent::auth_method::LEGACY_ECHO_AGENT_API_KEY_ENV_VAR,
    "ECHO_AGENT_AUTH",
    "ECHO_AGENT_AUTH_PATH",
    "ECHO_AGENT_DEPLOYMENT_KEY",
    "ECHO_AGENT_EXTRA_AUTH_KEY",
    "ECHO_AGENT_TRACE_UPLOAD_CREDENTIALS_FILE",
    "OTEL_EXPORTER_OTLP_HEADERS",
    "ECHO_AGENT_INTERNAL_OTLP_HEADERS",
];
/// Read an env var as a trimmed string. Returns `None` if unset or empty/whitespace-only.
pub(crate) fn env_string(name: &str) -> Option<String> {
    let value = std::env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}
pub use echo_agent_config::env_bool;
/// Compaction-mode precedence (env > config > remote settings > default, with
/// unrecognized values at each source falling through). `remote` sits just
/// above the default, mirroring `feature_flag` in `resolve_bool_flag`. Pure so
/// it's unit-testable without mutating process env.
fn resolve_compaction_mode_from(
    env: Option<&str>,
    config: Option<&str>,
    remote: Option<&str>,
) -> echo_agent_chat_state::CompactionMode {
    use echo_agent_chat_state::CompactionMode;
    env.and_then(CompactionMode::parse)
        .or_else(|| config.and_then(CompactionMode::parse))
        .or_else(|| remote.and_then(CompactionMode::parse))
        .unwrap_or_default()
}
/// Compaction-detail precedence (env > config > remote settings > default). Pure.
/// Controls the per-turn verbatim detail in `segments` mode (default `verbose`).
fn resolve_compaction_detail_from(
    env: Option<&str>,
    config: Option<&str>,
    remote: Option<&str>,
) -> echo_agent_chat_state::CompactionDetail {
    use echo_agent_chat_state::CompactionDetail;
    env.and_then(CompactionDetail::parse)
        .or_else(|| config.and_then(CompactionDetail::parse))
        .or_else(|| remote.and_then(CompactionDetail::parse))
        .unwrap_or_default()
}
/// Resolve a single vendor-compat cell: env > `[compat]` TOML > remote settings
/// remote flag > default ON.
fn resolve_compat_cell(
    env: &str,
    cfg: Option<bool>,
    remote: Option<bool>,
    default: bool,
) -> Resolved<bool> {
    resolve_compat_cell_with_env(echo_agent_config::env_bool(env), cfg, remote, default)
}
pub(crate) fn resolve_compat_cell_with_env(
    env: Option<bool>,
    cfg: Option<bool>,
    remote: Option<bool>,
    default: bool,
) -> Resolved<bool> {
    if let Some(value) = env {
        Resolved::new(value, ConfigSource::Env)
    } else if let Some(value) = cfg {
        Resolved::new(value, ConfigSource::Config)
    } else if let Some(value) = remote {
        Resolved::new(value, ConfigSource::Remote)
    } else {
        Resolved::new(default, ConfigSource::Default)
    }
}
fn remote_compat_value(
    remote: Option<&crate::util::config::RemoteSettings>,
    key: Option<CompatRemoteKey>,
) -> Option<bool> {
    let remote = remote?;
    match key? {
        CompatRemoteKey::CursorSkills => remote.cursor_skills_enabled,
        CompatRemoteKey::CursorRules => remote.cursor_rules_enabled,
        CompatRemoteKey::CursorAgents => remote.cursor_agents_enabled,
        CompatRemoteKey::CursorMcps => remote.cursor_mcps_enabled,
        CompatRemoteKey::CursorHooks => remote.cursor_hooks_enabled,
        CompatRemoteKey::CursorSessions => remote.cursor_sessions_enabled,
        CompatRemoteKey::ClaudeSkills => remote.claude_skills_enabled,
        CompatRemoteKey::ClaudeRules => remote.claude_rules_enabled,
        CompatRemoteKey::ClaudeAgents => remote.claude_agents_enabled,
        CompatRemoteKey::ClaudeMcps => remote.claude_mcps_enabled,
        CompatRemoteKey::ClaudeHooks => remote.claude_hooks_enabled,
        CompatRemoteKey::ClaudeSessions => remote.claude_sessions_enabled,
        CompatRemoteKey::CodexSessions => remote.codex_sessions_enabled,
    }
}
/// Resolve vendor compatibility cells from TOML and remote settings.
fn resolve_compat_config(
    config: &CompatConfigToml,
    remote: Option<&crate::util::config::RemoteSettings>,
) -> CompatConfig {
    let defaults = CompatConfig::default();
    let mut resolved = defaults;
    for cell in COMPAT_CELLS {
        resolved.set(
            cell,
            resolve_compat_cell(
                cell.env_var(),
                config.value(cell),
                remote_compat_value(remote, cell.remote_key()),
                defaults.value(cell),
            )
            .value,
        );
    }
    resolved
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompatConfigCellError {
    Unavailable,
    Malformed,
}
pub(crate) fn compat_config_cell(
    raw_config: Result<&toml::Value, ()>,
    cell: echo_agent_tools::types::compat::CompatCell,
) -> Result<Option<bool>, CompatConfigCellError> {
    let raw = raw_config.map_err(|()| CompatConfigCellError::Unavailable)?;
    let Some(compat) = raw.get("compat") else {
        return Ok(None);
    };
    let compat = compat.as_table().ok_or(CompatConfigCellError::Malformed)?;
    let Some(vendor) = compat.get(cell.vendor().as_str()) else {
        return Ok(None);
    };
    let vendor = vendor.as_table().ok_or(CompatConfigCellError::Malformed)?;
    let Some(value) = vendor.get(cell.surface().as_str()) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or(CompatConfigCellError::Malformed)
}
/// Resolve only picker-facing session cells from raw config independently.
pub fn resolve_compat_sessions_from_raw(
    raw_config: Result<&toml::Value, ()>,
    remote: Option<&crate::util::config::RemoteSettings>,
) -> CompatConfig {
    let mut config = CompatConfigToml::default();
    for cell in COMPAT_CELLS
        .into_iter()
        .filter(|cell| cell.surface() == CompatSurface::Sessions)
    {
        let value = match compat_config_cell(raw_config, cell) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    vendor = cell.vendor().as_str(),
                    ?error,
                    "invalid compat config; disabling foreign sessions"
                );
                Some(false)
            }
        };
        match cell.vendor() {
            CompatVendor::Cursor => config.cursor.sessions = value,
            CompatVendor::Claude => config.claude.sessions = value,
            CompatVendor::Codex => config.codex.sessions = value,
        }
    }
    resolve_compat_config(&config, remote)
}
/// Resolve a string setting: cli > env > config > feature flag. `None` if no source provides a value.
pub(crate) fn resolve_string_flag(
    cli_arg: Option<&str>,
    env_var: &str,
    config_val: Option<&str>,
    feature_flag_val: Option<&str>,
) -> Option<Resolved<String>> {
    if let Some(val) = cli_arg.filter(|s| !s.is_empty()) {
        return Some(Resolved::new(val.to_owned(), ConfigSource::Cli));
    }
    if let Some(val) = env_string(env_var) {
        return Some(Resolved::new(val, ConfigSource::Env));
    }
    if let Some(val) = config_val.filter(|s| !s.is_empty()) {
        return Some(Resolved::new(val.to_owned(), ConfigSource::Config));
    }
    if let Some(val) = feature_flag_val.filter(|s| !s.is_empty()) {
        return Some(Resolved::new(val.to_owned(), ConfigSource::Remote));
    }
    None
}
/// Resolve `enabled` for section-based configs (memory, subagents, etc.).
/// Feature flag only applies when the TOML section is absent.
pub(crate) fn resolve_enabled(
    cli_flag: Option<bool>,
    env_var: &str,
    config_enabled: bool,
    has_local_section: bool,
    feature_flag_val: Option<bool>,
    default: bool,
) -> Resolved<bool> {
    let config_val = if has_local_section {
        Some(config_enabled)
    } else {
        None
    };
    BoolFlag::env(env_var)
        .cli(cli_flag)
        .config(config_val)
        .feature_flag(feature_flag_val)
        .default(default)
        .resolve()
}
pub(crate) use echo_agent_telemetry::config::env_telemetry_mode;
pub use echo_agent_telemetry::config::{TelemetryConfig, TelemetryMode};
/// Plugin system configuration from `[plugins]` section in config.toml.
///
/// ```toml
/// [plugins]
/// paths = ["~/my-plugins/custom-tools"]
/// disabled = ["user/a1b2c3d4/noisy-plugin"]
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PluginsConfig {
    /// Additional plugin directory paths to load.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Plugin IDs or names to disable. Disabled plugins are discovered
    /// but their components are not loaded into the session.
    #[serde(default)]
    pub disabled: Vec<String>,
    /// Plugin IDs or names to explicitly enable. Used for project-scope plugins
    /// which are disabled by default — adding a plugin here overrides that default.
    #[serde(default)]
    pub enabled: Vec<String>,
    /// CLI `--plugin-dir` paths (populated by CLI arg processing, not config file).
    #[serde(skip)]
    pub cli_plugin_dirs: Vec<std::path::PathBuf>,
}
impl PluginsConfig {
    /// Merge `enabledPlugins` from Claude settings files into this config.
    ///
    /// Reads `enabledPlugins` from `~/.claude/settings.json` only (user scope).
    /// Project-level `<git_root>/.claude/settings.json` is intentionally NOT
    /// read here: a malicious repo could pre-populate `enabledPlugins` to
    /// bypass the project-plugin auto-disable logic in `populate_plugin_lists`,
    /// enabling attacker-controlled hooks (e.g. SessionStart → RCE).
    /// Native `.echo-agent/config.toml` entries already present take precedence:
    /// a name is only added if it isn't already in the opposite list.
    pub(crate) fn merge_claude_enabled_plugins(&mut self, _cwd: Option<&std::path::Path>) {
        if crate::claude_import::is_claude_import_marked_with_log("merge_claude_enabled_plugins") {
            return;
        }
        let mut paths = Vec::new();
        if let Some(home) = echo_agent_config::discovery_home_dir() {
            paths.push(home.join(".claude").join("settings.json"));
        }
        for path in &paths {
            let (claude_enabled, claude_disabled) =
                echo_agent_core::plugins::marketplace::load_enabled_disabled_plugins(path);
            for name in claude_enabled {
                if !self.disabled.contains(&name) && !self.enabled.contains(&name) {
                    self.enabled.push(name);
                }
            }
            for name in claude_disabled {
                if !self.enabled.contains(&name) && !self.disabled.contains(&name) {
                    self.disabled.push(name);
                }
            }
        }
    }
    /// Build a `DiscoveryConfig` from this plugins config.
    pub(crate) fn to_discovery_config(
        &self,
    ) -> echo_agent_core::plugins::discovery::DiscoveryConfig {
        let (disabled_marketplace_sources, disabled_marketplace_source_aliases) =
            crate::plugin::marketplace_source_revocations();
        echo_agent_core::plugins::discovery::DiscoveryConfig {
            cli_plugin_dirs: self.cli_plugin_dirs.clone(),
            config_paths: self.paths.iter().map(std::path::PathBuf::from).collect(),
            disabled: self.disabled.clone(),
            enabled: self.enabled.clone(),
            disabled_marketplace_sources,
            disabled_marketplace_source_aliases,
        }
    }
}
/// Feedback submission configuration (`[feedback]` in config.toml).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedbackConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<FeedbackUserConfig>,
}
/// Self-reported feedback author identity (never used for authorization).
/// Merged only from trusted config tiers, so a cloned repo can't inject the
/// `command` escape hatch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedbackUserConfig {
    /// Sources tried in order for the name. `os_user` yields the OS user name;
    /// any other entry is a literal (`$VAR` expanded at load).
    pub name: Vec<String>,
    /// Sources tried in order for the email. `git_email` yields the global git
    /// email; any other entry is a literal (`$VAR` expanded at load) needing `@`.
    pub email: Vec<String>,
    /// Fallback domain for `<name>@<domain>` when no `email` source resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email_domain: Option<String>,
    /// Optional `sh -c` script printing `{"name","email"}` JSON; its fields win
    /// over the lists above, with per-field fallback. Trusted config tiers only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CompactionConfig {
    pub memory_flush: Option<crate::config::MemoryFlushSettings>,
    pub pruning: Option<crate::config::PruningSettings>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CliConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissed_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub npm_registry: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_leader: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_tips: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worktree_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_registry: Option<bool>,
    /// Env `ECHO_AGENT_MINIMUM_VERSION`. See [`crate::util::config::VersionPolicy`] for
    /// the version-policy knobs. (Unrelated to
    /// `version_overrides[].maximum_version`, which gates config patches.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum_version: Option<String>,
    /// Env `ECHO_AGENT_MAXIMUM_VERSION`. See [`crate::util::config::VersionPolicy`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_version: Option<String>,
    /// Env `ECHO_AGENT_REQUIRED_MINIMUM_VERSION`. See [`crate::util::config::VersionPolicy`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_minimum_version: Option<String>,
    /// Env `ECHO_AGENT_REQUIRED_MAXIMUM_VERSION`. See [`crate::util::config::VersionPolicy`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_maximum_version: Option<String>,
    /// Group sessions by repo in the picker and CLI listings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_picker_grouped: Option<bool>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosticsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crash_handler: Option<bool>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
    /// The pre-campaign `models.default` (merged user/managed/requirements)
    /// captured when a campaign is overriding the default, so model resolution can
    /// recover if the campaign points at a model missing from the catalog. `None`
    /// when there is nothing to recover to. Runtime-only; never serialized.
    #[serde(skip)]
    pub pre_campaign_default: Option<String>,
    /// Whether an active campaign is currently overriding `models.default`. The
    /// authoritative campaign-driven-default signal (set from the resolved active
    /// set), correct even when the user has no base default. Runtime-only.
    #[serde(skip)]
    pub default_is_campaign_driven: bool,
    /// Persisted effort for the default model; applied in `resolve_model_catalog`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_summary: Option<String>,
    /// Vision model used to transcribe user-supplied
    /// images via a separate endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_description: Option<String>,
    /// Model pin for next-prompt suggestions (tab-autocomplete ghost text).
    /// Unset = remote pin, then the client hint / built-in `echo-agent-build-0.1`
    /// default with the catalog guard; see `ModelOverrideConfig::resolve`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_suggestion: Option<String>,
    /// Restricts which models are user-selectable for normal chat (picker,
    /// `/model`, `-m`). Non-matching models stay in the catalog but are never
    /// shown, defaulted to, or selectable. Special/internal models (web_search,
    /// image_description, subagents, fork secondary) are exempt.
    ///
    /// Glob patterns (`*`, `?`, `[...]`) match the model id or catalog key,
    /// case-sensitive. Empty = no restriction; an excluded explicit `default`/`-m`
    /// is rejected at startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Force `hidden = true` on these model IDs (still usable via `-m`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_models: Option<Vec<String>>,
    /// Remove these model IDs from the catalog entirely. Wins over `hidden_models`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_models: Option<Vec<String>>,
    /// Fallback `agent_type` for models without a per-model override.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    /// Global default request headers applied to every model. A per-model
    /// `[model.<id>].extra_headers` entry overrides per key (case-insensitive).
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub extra_headers: IndexMap<String, String>,
    /// Global default values applied to every model that leaves the field
    /// unset; a per-model `[model.<id>]` value always wins. A deliberately
    /// small, allow-listed subset of the per-model fields (only `Option` ones,
    /// so "unset" is unambiguous). Future: these could consolidate into a
    /// `[models.defaults]` sub-table mirroring the per-model schema 1:1; kept
    /// flat for now as that is a larger refactor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inference_idle_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_rate_limit_max_attempts: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_tool_calls: Option<bool>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HarnessConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_for_upload: Option<bool>,
    /// Budget (seconds) for the turn-end upload flush when
    /// `block_for_upload` is active. Default 60.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload_flush_timeout_secs: Option<u64>,
}
impl HarnessConfig {}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RelayConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}
/// `[hub]` section from config.toml.
///
/// Optional default Computer Hub URL for **workspace provider** exposure
/// (`echoagent workspace` / leader `with_default_hub_url`). Does **not** enable
/// agent-side harness/client connections or alter local session behavior.
///
/// ```toml
/// [hub]
/// url = "wss://hub.echo-agent.invalid/ws"
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HubConfig {
    /// Hub WebSocket URL (`ws://` or `wss://`) used as the leader default for
    /// `echoagent workspace start` when the CLI does not pass `--hub-url`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}
impl HubConfig {
    /// Whether a non-empty hub URL is configured (workspace default only).
    pub fn is_enabled(&self) -> bool {
        self.url.as_ref().is_some_and(|u| !u.trim().is_empty())
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorktreePoolConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_size: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_count_threshold: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<usize>,
}
/// `[worktree]` section from config.toml (auto-GC policy lives under `auto_gc`).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorktreeConfigSection {
    #[serde(default)]
    pub auto_gc: crate::util::config::WorktreeAutoGcSettings,
}
/// `[sandbox]` section from config.toml.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxSettingsConfig {
    /// "off", "workspace", "devbox", "read-only", "strict", or custom name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Skip bash permission prompts when sandbox is active.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_allow_bash: Option<bool>,
}
impl SandboxSettingsConfig {
    pub(crate) fn from_effective_config() -> Self {
        crate::config::load_effective_config()
            .ok()
            .and_then(|v| v.get("sandbox")?.clone().try_into().ok())
            .unwrap_or_default()
    }
    /// Resolve sandbox profile: requirement > CLI > env > config > "off".
    pub fn resolve_profile(
        &self,
        cli_arg: Option<&str>,
        requirement: Option<&str>,
    ) -> Resolved<String> {
        if let Some(val) = requirement {
            return Resolved::new(val.to_owned(), ConfigSource::Requirement);
        }
        resolve_string_flag(cli_arg, "ECHO_AGENT_SANDBOX", self.profile.as_deref(), None)
            .unwrap_or_else(|| Resolved::new("off".to_owned(), ConfigSource::Default))
    }
    /// Resolve auto_allow_bash: requirement > env > config > default (false).
    pub(crate) fn resolve_auto_allow_bash(&self, requirement: Option<bool>) -> Resolved<bool> {
        BoolFlag::env("ECHO_AGENT_SANDBOX_AUTO_ALLOW_BASH")
            .requirement(requirement)
            .config(self.auto_allow_bash)
            .resolve()
    }
}
/// `[marketplace]` section from config.toml (plugin marketplace sources).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct MarketplaceConfig {
    /// `[[marketplace.sources]]` entries.
    #[serde(default)]
    pub sources: Vec<MarketplaceSourceEntry>,
    /// Source identities revoked in this Echo profile. External compatibility
    /// settings and installed files remain read-only discovery inputs.
    #[serde(default)]
    pub disabled_sources: Vec<String>,
    /// Local logical alias -> canonical identity captured at revocation.
    #[serde(default)]
    pub disabled_source_aliases: std::collections::BTreeMap<String, String>,
    /// Written/read out-of-band by `extensions::marketplace`, opaque so a wrong-typed value can't fail load.
    #[serde(default)]
    pub official_marketplace_auto_installed: Option<toml::Value>,
    /// Read out-of-band by the pager (plugin-CTA marketplace override), opaque so a wrong-typed value can't fail load.
    #[serde(default)]
    pub plugin_cta_marketplace: Option<toml::Value>,
    /// Written/read out-of-band by `extensions::marketplace`, opaque so a wrong-typed value can't fail load.
    #[serde(default)]
    pub default_skills_installs_purged: Option<toml::Value>,
}
/// A single `[[marketplace.sources]]` entry.
#[derive(Clone, Debug, Deserialize)]
pub struct MarketplaceSourceEntry {
    pub name: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub git: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
}
/// `[storage]` section from config.toml.
///
/// Controls session persistence settings like cleanup TTL.
/// Read by `resolve_cleanup_ttl_days()` in `session/persistence.rs`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Number of days to keep stale sessions before cleanup. Default: 30.
    pub cleanup_ttl_days: Option<u32>,
}
/// `[paths]` configuration: extra directories to scan for skills, rules, etc.
///
/// These supplement the built-in scan locations (`.echo-agent/skills/`,
/// `.agents/skills/`, `~/.echo-agent/skills/`). They're written by `/import-claude`
/// to preserve previously-discovered Claude directories after the runtime
/// `.claude/` cutoff (see `[claude_compat] imported`).
///
/// Example:
/// ```toml
/// [paths]
/// extra_skill_dirs = ["~/.claude/skills", "/path/to/.claude/skills"]
/// extra_rule_dirs = ["~/.claude/rules"]
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    /// Additional directories to scan for skills (each contains `<skill>/SKILL.md`).
    pub extra_skill_dirs: Vec<String>,
    /// Additional directories to scan for rules (each contains `*.md`).
    pub extra_rule_dirs: Vec<String>,
}
/// `[permission]` known keys, declared for the unrecognized-key scan only;
/// consumed out-of-band. Keys stay typed so a typo (e.g. `denny`) still warns.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct PermissionKnownKeys {
    /// Compact rule arrays (`parse_toml_permission_section`).
    pub allow: Option<toml::Value>,
    pub deny: Option<toml::Value>,
    pub ask: Option<toml::Value>,
    /// Verbose `[[permission.rules]]` form.
    pub rules: Option<toml::Value>,
}
/// `[shell_environment_policy]` known keys, for the unrecognized-key scan only;
/// the value is parsed at spawn by [`crate::util::config::resolve_shell_env_policy`].
/// `Option<toml::Value>` (no `deny_unknown_fields`) keeps a typo a warning, not a
/// load failure, like [`PermissionKnownKeys`].
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct ShellEnvironmentPolicyKnownKeys {
    pub inherit: Option<toml::Value>,
    pub ignore_default_excludes: Option<toml::Value>,
    pub exclude: Option<toml::Value>,
    pub set: Option<toml::Value>,
    pub include_only: Option<toml::Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub features: Features,
    /// `[goal]` section: canonical `/goal` configuration. See [`GoalConfig`].
    #[serde(default)]
    pub goal: GoalConfig,
    #[serde(default)]
    pub workflows: WorkflowsConfig,
    /// `[doom_loop_recovery]` section: the shared settings struct — ONE type
    /// serves this TOML table and the remote remote settings `doom_loop_recovery`
    /// object. See [`crate::util::config::DoomLoopRecoverySettings`].
    #[serde(default)]
    pub doom_loop_recovery: crate::util::config::DoomLoopRecoverySettings,
    /// `[worktree]` section (currently `[worktree.auto_gc]` only).
    #[serde(default)]
    pub worktree: WorktreeConfigSection,
    /// `[auto_mode]` section: Auto permission-mode configuration. See [`AutoModeConfig`].
    #[serde(default)]
    pub auto_mode: AutoModeConfig,
    /// What `[features]` said in the merged layers. One tier of
    /// [`Config::feature`].
    #[serde(skip)]
    pub(crate) feature_values: BTreeMap<Feature, bool>,
    /// `[model.*]` overrides from config.toml. Resolve via `resolve_model_list()`.
    #[serde(skip)]
    pub config_models: IndexMap<String, ConfigModelOverride>,
    #[serde(skip)]
    pub config_warnings: Vec<super::config_model_override_parse::ConfigWarning>,
    pub echo_agent_com_config: EchoAgentComConfig,
    /// `[auth_provider.<name>]` tables, populated by
    /// [`parse_auth_providers`] from trusted config layers only.
    #[serde(skip)]
    pub auth_providers: IndexMap<String, crate::auth::AuthProviderConfig>,
    #[serde(skip)]
    pub model_providers: IndexMap<String, ModelProviderConfig>,
    /// Written by the client via `config_toml_edit`; absorbed so it isn't
    /// flagged as an unrecognized key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hints: Option<toml::Value>,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub toolset: ShellToolsetConfig,
    /// Validation only; the value is parsed at spawn by `resolve_shell_env_policy`.
    #[serde(default, skip_serializing)]
    pub shell_environment_policy: ShellEnvironmentPolicyKnownKeys,
    #[serde(default)]
    pub endpoints: EndpointsConfig,
    #[serde(default)]
    pub telemetry: TelemetryConfig,
    /// Session behavior configuration.
    #[serde(default)]
    pub session: SessionConfig,
    /// Agent definition selection configuration.
    /// Set in `config.toml` under `[agent]` to choose which agent definition
    /// is used for all sessions (unless overridden by CLI flag or ACP meta).
    #[serde(default)]
    pub agent: AgentSelectionConfig,
    #[serde(default)]
    pub repo_changes_dedup: RepoChangesDedupConfig,
    /// Skills discovery configuration.
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Raw `[compat]` vendor-compatibility config (per-vendor × per-surface
    /// toggles). Resolved into [`Config::compat_resolved`] by
    /// `resolve_runtime_fields`.
    #[serde(default)]
    pub compat: CompatConfigToml,
    /// Plugin system configuration.
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// Feedback submission configuration.
    #[serde(default)]
    pub feedback: FeedbackConfig,
    /// Filesystem path overrides (`[paths]` in config.toml).
    #[serde(default)]
    pub paths: PathsConfig,
    #[serde(default, skip_serializing)]
    pub cli: CliConfig,
    #[serde(default, skip_serializing)]
    pub models: ModelsConfig,
    #[serde(default, skip_serializing)]
    pub harness: HarnessConfig,
    #[serde(default, skip_serializing)]
    pub relay: RelayConfig,
    /// Computer Hub configuration (`[hub]` in config.toml).
    #[serde(default, skip_serializing)]
    pub hub: HubConfig,
    #[serde(default, skip_serializing)]
    pub worktree_pool: WorktreePoolConfig,
    #[serde(default, skip_serializing)]
    pub sandbox: SandboxSettingsConfig,
    #[serde(default, skip_serializing)]
    pub mcp_servers: std::collections::HashMap<String, crate::util::config::McpServerConfig>,
    #[serde(default, skip_serializing)]
    pub disabled_mcp_servers: Vec<String>,
    #[serde(default, skip_serializing)]
    pub disabled_mcp_tools: std::collections::HashMap<String, Vec<String>>,
    #[serde(default, skip_serializing)]
    pub subagents: crate::config::SubagentsConfig,
    #[serde(default, skip_serializing)]
    pub memory: crate::config::MemorySettings,
    #[serde(default, skip_serializing)]
    pub compaction: CompactionConfig,
    #[serde(default, skip_serializing)]
    pub managed_mcps: crate::config::ManagedMcpsConfig,
    /// `[auth]` alias — consumed by `expand_auth_alias` before serde.
    /// Typed as `EchoAgentComConfig` (same schema) so sub-field typos are caught.
    #[serde(default, skip_serializing)]
    pub auth: Option<EchoAgentComConfig>,
    /// `[desktop]` section — owned by echo-agent-desktop (Electron app), opaque to the CLI agent.
    #[serde(default, skip_serializing)]
    pub desktop: Option<toml::Value>,
    /// Top-level `announcements` array — consumed by `resolve_announcements`.
    #[serde(default, skip_serializing)]
    pub announcements: Vec<echo_agent_announcements::RemoteAnnouncement>,
    /// `[tips]` section — consumed by `merge_tips`.
    #[serde(default, skip_serializing)]
    pub tips: Option<crate::util::config::TipsOverride>,
    /// `[permission]` — consumed out-of-band; see [`PermissionKnownKeys`].
    #[serde(default, skip_serializing)]
    pub permission: PermissionKnownKeys,
    /// `[tools]` — also read by `ToolsConfig::resolve()`.
    #[serde(default, skip_serializing)]
    pub tools: crate::config::ToolsConfig,
    /// `[storage]` — also read by `resolve_cleanup_ttl_days()`.
    #[serde(default, skip_serializing)]
    pub storage: StorageConfig,
    /// `[marketplace]` — also read by `echo_agent_plugin_marketplace::load_sources()`.
    #[serde(default, skip_serializing)]
    pub marketplace: MarketplaceConfig,
    /// `[diagnostics]` — crash handler toggle (`load_crash_handler_enabled_sync`).
    #[serde(default, skip_serializing)]
    pub diagnostics: DiagnosticsConfig,
    /// Storage mode for session persistence.
    /// When running in relay/headless mode, this should be set to Writeback.
    /// Defaults to reading from ECHO_AGENT_STORAGE_MODE env var.
    #[serde(skip)]
    pub storage_mode: StorageMode,
    /// CLI override for the default model ID.
    #[serde(skip)]
    pub default_model_override: Option<String>,
    /// CLI override for reasoning effort.
    #[serde(skip)]
    pub reasoning_effort_override: Option<ReasoningEffort>,
    /// CLI override for the web search model ID.
    #[serde(skip)]
    pub web_search_model_override: Option<String>,
    /// CLI override for the session summary model ID.
    #[serde(skip)]
    pub session_summary_model_override: Option<String>,
    /// CLI override for YOLO mode (auto-approve all permissions).
    /// Takes precedence over default settings.
    #[serde(skip)]
    pub default_yolo_mode: bool,
    /// Start sessions in auto permission mode (classifier) when no per-session override.
    pub default_auto_mode: bool,
    /// CLI memory override preserved across config and remote-setting refreshes.
    #[serde(skip)]
    pub memory_enabled_override: Option<bool>,
    /// Original CLI `--subagents` tri-state, preserved for re-resolution
    /// when remote settings settings are refreshed on /new.
    #[serde(skip)]
    pub cli_subagents: Option<bool>,
    /// Resolved memory configuration. `None` when memory is disabled.
    /// Resolved by [`RuntimeResolutionContext`] in [`Config::resolve_runtime_fields`].
    #[serde(skip)]
    pub memory_config: Option<crate::config::MemoryConfig>,
    /// CLI override: path to an agent profile (.md file with YAML frontmatter).
    #[serde(skip)]
    pub agent_profile_path: Option<PathBuf>,
    /// Client version string (e.g., "0.1.77 (abc1234)").
    /// Set by the TUI/CLI launcher and used as fallback when clients don't provide clientVersion.
    #[serde(skip)]
    pub client_version: Option<String>,
    /// The mode in which the agent is running.
    /// Determines behavior like relay sync enablement (only enabled in TUI mode).
    #[serde(skip)]
    pub mode: AgentMode,
    /// Remote settings fetched from cli-chat-proxy at startup.
    /// Used for upload limits (replaces on-demand /v1/storage/limits fetch).
    #[serde(skip)]
    pub remote_settings: Option<crate::util::config::RemoteSettings>,
    #[serde(skip)]
    pub cli_agents: Vec<echo_agent_core::config::AgentDefinition>,
    #[serde(skip)]
    pub cli_agent_overrides: CliAgentOverrides,
    /// Whether subagent (task tool) support is enabled. Enabled by default;
    /// disabled only via `ECHO_AGENT_SUBAGENTS=0` or `[subagents] enabled = false`.
    /// Not remotely gated.
    #[serde(skip)]
    pub subagents_enabled: bool,
    /// Resolved max subagent nesting depth (see
    /// [`crate::config::SubagentsConfig::resolve_max_depth`]).
    #[serde(skip)]
    pub subagents_max_depth: u32,
    #[serde(skip)]
    pub subagents_max_concurrent: usize,
    /// Resolved concurrent subagent turn-sampling limit feeding the shared
    /// semaphore. See [`crate::config::SubagentsConfig::resolve_sampling_limit`].
    #[serde(skip)]
    pub subagents_sampling_limit: usize,
    #[serde(skip)]
    pub subagents_limit_behavior:
        echo_agent_tools::implementations::echo_agent_build::task::admission::LimitBehavior,
    #[serde(skip)]
    pub workflow_max_concurrent_agents: usize,
    #[serde(skip)]
    pub media_gen_batch_limits: echo_agent_tools::media_gen_limits::MediaGenBatchLimits,
    /// Per-subagent model ID overrides from `[subagents.models]` in config.toml.
    /// Keys are agent names, values are model IDs. Set alongside `subagents_enabled`
    /// from `SubagentsConfig::resolve()`.
    #[serde(skip)]
    pub subagent_model_overrides: std::collections::HashMap<String, String>,
    /// Per-subagent enable/disable toggles from `[subagents.toggle]` in config.toml.
    /// Keys are agent names, values are booleans. Omitted agents default to enabled.
    #[serde(skip)]
    pub subagent_toggle: std::collections::HashMap<String, bool>,
    /// Trust-independent roles from inline, user, and bundled sources.
    #[serde(skip)]
    pub subagent_roles:
        std::collections::HashMap<String, echo_agent_subagent_resolution::config::SubagentRole>,
    /// Trust-independent personas from inline, user, and bundled sources.
    #[serde(skip)]
    pub subagent_personas:
        std::collections::HashMap<String, echo_agent_subagent_resolution::config::SubagentPersona>,
    /// Whether web search is force-disabled via `--disable-web-search` CLI flag.
    /// When true, the web search tool is never added to the agent toolset
    /// regardless of available credentials.
    #[serde(default)]
    pub disable_web_search: bool,
    /// Whether the runtime turn-end TodoGate is force-enabled via the
    /// `--todo-gate` CLI flag. Session-scoped — not persisted. When
    /// true, flips the runtime policy's `enabled` bit on regardless of
    /// remote settings or the built-in default (which is `false`).
    /// The gate runs only while a `/goal` is active (goal reminders
    /// inject `<task_completion_discipline>`); global built-in templates
    /// do not activate it.
    #[serde(skip)]
    pub todo_gate: bool,
    /// Path for the Layer-3 LazinessDetector debug log
    /// (`--laziness-debug-log`). When `Some`, the classifier fires
    /// after every turn (bypassing the idle wait, the per-model
    /// enable gate, and the nudge cap) and appends a JSONL line per
    /// fire to this file. Observation-only — no nudges are injected
    /// in this mode. Session-scoped, not persisted.
    #[serde(skip)]
    pub laziness_debug_log: Option<std::path::PathBuf>,
    /// Whether tools should respect `.gitignore` patterns.
    /// When `true`, all tools including `read_file` block gitignored files.
    /// When `false` (default), each tool applies its own default
    /// (`read_file` allows, others block).
    /// Resolved by [`crate::config::ToolsConfig::resolve`].
    #[serde(skip)]
    pub respect_gitignore: bool,
    /// When `true` (and no valid `zdr_video_output_s3` bucket is set),
    /// `MvpAgent::prepare_video_gen_config` marks the video tools
    /// zdr-restricted: they stay advertised but short-circuit at call time
    /// with setup guidance. Resolved by [`crate::config::ToolsConfig::resolve`].
    #[serde(skip)]
    pub disable_zdr_incompatible_tools: bool,
    /// S3 config for ZDR video output (presigned upload to team bucket).
    /// Only used when `disable_zdr_incompatible_tools` is `true` and the
    /// config is valid. Resolved by [`crate::config::ToolsConfig::resolve`].
    #[serde(skip)]
    pub zdr_video_output_s3: Option<
        echo_agent_tools::implementations::echo_agent_build::video_gen::ZdrVideoOutputS3Config,
    >,
    /// Whether to enrich path-not-found errors with CWD reminders,
    /// "dropped repo folder" correction, and similar-name suggestions.
    /// Default `false`. Enabled via remote settings.
    /// Serialized to `config.json` on GCS so traces can distinguish
    /// which sessions had path-not-found hints active.
    #[serde(default)]
    pub path_not_found_hints: bool,
    /// Whether to fetch managed MCP configs from the managed connectors service at startup.
    /// Resolved by [`crate::config::ManagedMcpsConfig::resolve`]: env var >
    /// config.toml > remote settings > default (off in headless, on in interactive).
    #[serde(skip)]
    pub managed_mcps_enabled: bool,
    #[serde(skip)]
    pub managed_mcp_gateway_tools_enabled: bool,
    /// Resolved vendor-compat config (env → `[compat]` TOML → feature flag →
    /// default ON), built from `compat` + `remote_settings` in
    /// `resolve_runtime_fields`. Threaded into skills / rules / AGENTS.md
    /// discovery.
    #[serde(skip)]
    pub compat_resolved: CompatConfig,
    /// Enforced requirement pins from `requirements.toml`.
    #[serde(skip)]
    pub requirements: Requirements,
    /// Model ID for web_search.
    #[serde(skip)]
    pub web_search_model: String,
    /// Session title model. Resolved to the compiled default
    /// (`default_session_summary_model`) when unset; see `ModelOverrideConfig::resolve`.
    #[serde(skip)]
    pub session_summary_model: Option<String>,
    /// Image describe model (`echo-agent-build` default via `ModelOverrideConfig::resolve`).
    #[serde(skip)]
    pub image_description_model: Option<String>,
    /// Next-prompt suggestion model pin (`env > [models] prompt_suggestion >
    /// remote`), consumed catalog-guarded by `handle_suggest_prompt`; see
    /// `ModelOverrideConfig::resolve`.
    #[serde(skip)]
    pub prompt_suggest_model_pin: crate::config::PromptSuggestModelPin,
}
#[derive(Debug, Clone, Default)]
pub struct CliAgentOverrides {
    pub tools: Option<Vec<String>>,
    pub disallowed_tools: Option<Vec<String>>,
    pub permission_rules: Vec<echo_agent_workspace::permission::types::PermissionRule>,
    pub max_turns: Option<u32>,
    pub permission_mode: Option<echo_agent_core::config::PermissionMode>,
}
impl CliAgentOverrides {
    /// Apply to the *main-session* agent, which the operator defines directly:
    /// the flags are authoritative, so they replace the agent's own fields.
    /// Spawned subagents instead layer these on top of an author's definition —
    /// see [`Self::apply_to_subagent_definition`].
    pub(crate) fn apply_to_definition(&self, def: &mut echo_agent_core::config::AgentDefinition) {
        if let Some(ref tools) = self.tools {
            def.tools = tools.clone();
        }
        if let Some(ref dt) = self.disallowed_tools {
            def.disallowed_tools = dt.clone();
        }
        if let Some(ref pm) = self.permission_mode {
            def.permission_mode = pm.clone();
        }
    }
    /// Subagent variant of [`Self::apply_to_definition`]: records the flags as
    /// session-clamp state (see [`AgentDefinition::session_tools_allowlist`])
    /// instead of overwriting the agent author's own fields.
    pub(crate) fn apply_to_subagent_definition(
        &self,
        def: &mut echo_agent_core::config::AgentDefinition,
    ) {
        def.session_tools_allowlist = self.tools.clone();
        def.session_tools_denylist = self.disallowed_tools.clone();
        if let Some(ref parent_mode) = self.permission_mode
            && def.plugin_name.is_none()
        {
            def.permission_mode =
                resolve_subagent_permission_mode(def.permission_mode.clone(), parent_mode);
        }
    }
    pub(crate) fn has_definition_overrides(&self) -> bool {
        self.tools.is_some() || self.disallowed_tools.is_some() || self.permission_mode.is_some()
    }
}
/// Parent bypassPermissions/acceptEdits/auto override the subagent's own mode
/// (spec); any other parent mode keeps it.
fn resolve_subagent_permission_mode(
    own: PermissionMode,
    parent: &PermissionMode,
) -> PermissionMode {
    match parent {
        PermissionMode::BypassPermissions | PermissionMode::AcceptEdits | PermissionMode::Auto => {
            parent.clone()
        }
        _ => own,
    }
}
pub use echo_agent_core::config::AgentDefinition;
pub use echo_agent_core::config::Effort;
pub use echo_agent_core::config::PermissionMode;
pub use echo_agent_shared::ui_config::{ContextualHints, UiConfig};
/// Configuration for selecting the agent definition.
///
/// Set in `config.toml` under `[agent]`:
///
/// ```toml
/// [agent]
/// # Use a named agent (looked up via discovery: .echo-agent/agents/, ~/.echo-agent/agents/, built-ins)
/// name = "my-custom-agent"
///
/// # OR: path to an agent definition file (.md with YAML frontmatter)
/// definition = "/path/to/my-agent.md"
/// ```
///
/// Priority (highest to lowest):
/// 1. ACP session-level `_meta.agentProfile`
/// 2. CLI `--agent-profile` flag
/// 3. `[agent]` config.toml section (this config)
/// 4. `ECHO_AGENT_AGENT` env var
/// 5. Default `echo-agent-build` agent
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSelectionConfig {
    /// Name of a built-in or discovered agent definition.
    /// Looked up via `echo_agent_core::discovery::by_name_in_cwd()`.
    /// Examples: "echo-agent-build", "browser-use", or a custom agent name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Path to an agent definition file (.md with YAML frontmatter).
    /// When set, the agent is loaded from this file.
    /// Supports environment variable expansion (e.g., `$HOME/.echo-agent/agents/my-agent.md`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub definition: Option<PathBuf>,
    /// Global system-prompt identity label. Per-model override wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_label: Option<String>,
}
/// Configuration for session behavior.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SessionConfig {
    /// Context window usage percentage (0-100) at which auto-compact is triggered.
    /// When the session's token usage exceeds this percentage of the model's context window,
    /// the conversation will be automatically summarized to free up space.
    ///
    /// `None` means "user didn't set it"; the resolver in
    /// `crate::util::config::resolve_auto_compact_threshold_percent` falls
    /// through to remote tiers and ultimately the hardcoded default 85.
    /// Read this field via the resolver — not directly — to honor the full
    /// precedence chain (env, per-model, remote, default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold_percent: Option<u8>,
    /// Whether to load environment variables from .envrc files.
    /// When enabled, the session will parse .envrc in the workspace directory
    /// and inject the environment variables into bash commands.
    /// Defaults to `true` when unset. `Option<bool>` so `None`
    /// round-trips as absent on disk (managed config wins over default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_envrc: Option<bool>,
}
/// Configuration for change-archive deduplication.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RepoChangesDedupConfig {
    pub enabled: bool,
    /// Include inline content even when references exist.
    pub include_inline_fallback: bool,
    /// Omit inline content larger than this (0 = no limit).
    pub max_inline_bytes: usize,
    /// Deduplicate untracked file content.
    pub dedup_untracked: bool,
    /// Deduplicate binary file blobs.
    pub dedup_binary: bool,
    /// Skip untracked files larger than this (0 = no limit).
    pub untracked_max_bytes: usize,
    /// Optional glob patterns to exclude untracked paths.
    pub untracked_exclude_globs: Vec<String>,
}
impl RepoChangesDedupConfig {}
impl Default for RepoChangesDedupConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            include_inline_fallback: false,
            max_inline_bytes: 0,
            dedup_untracked: true,
            dedup_binary: true,
            untracked_max_bytes: 0,
            untracked_exclude_globs: Vec::new(),
        }
    }
}
impl Default for Config {
    fn default() -> Self {
        let endpoints = EndpointsConfig::default();
        let mut cfg = Self {
            features: Features::default(),
            goal: GoalConfig::default(),
            workflows: WorkflowsConfig::default(),
            doom_loop_recovery: crate::util::config::DoomLoopRecoverySettings::default(),
            worktree: WorktreeConfigSection::default(),
            auto_mode: AutoModeConfig::default(),
            feature_values: BTreeMap::new(),
            config_models: IndexMap::new(),
            config_warnings: Vec::new(),
            echo_agent_com_config: EchoAgentComConfig::default(),
            auth_providers: IndexMap::new(),
            model_providers: IndexMap::new(),
            hints: None,
            ui: UiConfig::default(),
            toolset: ShellToolsetConfig::default(),
            shell_environment_policy: ShellEnvironmentPolicyKnownKeys::default(),
            endpoints,
            telemetry: TelemetryConfig::default(),
            session: SessionConfig::default(),
            agent: AgentSelectionConfig::default(),
            repo_changes_dedup: RepoChangesDedupConfig::default(),
            skills: SkillsConfig::default(),
            compat: CompatConfigToml::default(),
            plugins: PluginsConfig::default(),
            feedback: FeedbackConfig::default(),
            paths: PathsConfig::default(),
            cli: CliConfig::default(),
            models: ModelsConfig::default(),
            harness: HarnessConfig::default(),
            relay: RelayConfig::default(),
            hub: HubConfig::default(),
            worktree_pool: WorktreePoolConfig::default(),
            sandbox: SandboxSettingsConfig::default(),
            mcp_servers: std::collections::HashMap::new(),
            disabled_mcp_servers: Vec::new(),
            disabled_mcp_tools: std::collections::HashMap::new(),
            subagents: crate::config::SubagentsConfig::default(),
            memory: crate::config::MemorySettings::default(),
            compaction: CompactionConfig::default(),
            managed_mcps: crate::config::ManagedMcpsConfig::default(),
            auth: None,
            desktop: None,
            announcements: Vec::new(),
            tips: None,
            permission: PermissionKnownKeys::default(),
            tools: crate::config::ToolsConfig::default(),
            storage: StorageConfig::default(),
            marketplace: MarketplaceConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            storage_mode: StorageMode::resolve(None, None),
            default_model_override: None,
            reasoning_effort_override: None,
            web_search_model_override: None,
            session_summary_model_override: None,
            default_yolo_mode: false,
            default_auto_mode: false,
            agent_profile_path: None,
            client_version: Some(echo_agent_version::VERSION.to_string()),
            mode: AgentMode::default(),
            remote_settings: None,
            cli_agents: Vec::new(),
            cli_agent_overrides: CliAgentOverrides::default(),
            subagents_enabled: true,
            subagents_max_depth: crate::config::SubagentsConfig::DEFAULT_MAX_DEPTH,
            subagents_max_concurrent:
                echo_agent_tools::implementations::echo_agent_build::task::admission::DEFAULT_MAX_CONCURRENT,
            subagents_sampling_limit:
                echo_agent_tools::implementations::echo_agent_build::task::admission::DEFAULT_MAX_CONCURRENT,
            subagents_limit_behavior: Default::default(),
            workflow_max_concurrent_agents:
                crate::session::workflow::host_service::DEFAULT_WORKFLOW_MAX_CONCURRENT_AGENTS,
            media_gen_batch_limits: echo_agent_tools::media_gen_limits::MediaGenBatchLimits::default(
            ),
            subagent_model_overrides: std::collections::HashMap::new(),
            subagent_toggle: std::collections::HashMap::new(),
            subagent_roles: std::collections::HashMap::new(),
            subagent_personas: std::collections::HashMap::new(),
            disable_web_search: false,
            todo_gate: false,
            laziness_debug_log: None,
            respect_gitignore: false,
            disable_zdr_incompatible_tools: false,
            zdr_video_output_s3: None,
            path_not_found_hints: false,
            memory_enabled_override: None,
            cli_subagents: None,
            memory_config: None,
            managed_mcps_enabled: true,
            managed_mcp_gateway_tools_enabled: false,
            compat_resolved: CompatConfig::default(),
            requirements: Requirements::default(),
            web_search_model: crate::models::default_web_search_model().to_owned(),
            session_summary_model: None,
            image_description_model: None,
            prompt_suggest_model_pin: crate::config::PromptSuggestModelPin::Unpinned,
        };
        cfg.apply_env_overrides();
        cfg
    }
}
/// `[features]` booleans read straight off the raw TOML, with no [`Features`]
/// field. The catch-all in [`Features`] types every such key, so this list only
/// decides which of them are known enough not to warn. A key missing from it
/// costs a visible false alarm, not a silent hole in the check. `image_edit` is
/// left out on purpose, because only a pin sets it, so a plain entry in a
/// user's config stays an unrecognized key.
pub(crate) const UNMIRRORED_BOOLEAN_FEATURES: &[&str] = &[
    "campaigns",
    "remember_mode",
    "remote_fetch",
    "zdr_access_enabled",
];
/// A value written where a boolean was meant. Only these fail the load, so a
/// key carrying some later release's typed value does not stop an older build
/// from starting on the same config.
fn reads_as_a_boolean(value: &toml::Value) -> bool {
    match value {
        toml::Value::String(text) => {
            matches!(
                text.trim().to_ascii_lowercase().as_str(),
                "true" | "false" | "yes" | "no" | "on" | "off" | "1" | "0"
            )
        }
        toml::Value::Integer(number) => matches!(*number, 0 | 1),
        _ => false,
    }
}
/// No file is named: the load sees one merged document.
fn non_boolean_feature_error(path: &str, value: &toml::Value) -> String {
    let found = match value {
        toml::Value::String(text) => format!("the quoted \"{text}\""),
        toml::Value::Integer(number) => number.to_string(),
        other => other.type_str().to_owned(),
    };
    format!("{path}: expected true or false, found {found}")
}
/// Config paths read by raw-layer resolvers, not [`Config`] serde fields, so
/// `serde_ignored` must not report them as unrecognized keys.
const NON_SERDE_CONFIG_PATHS: &[&str] = &[crate::util::config::SLASH_COMMAND_TAGS_CONFIG_PATH];
/// [`NON_SERDE_CONFIG_PATHS`] plus the multi-path groups, every registered
/// feature, and every [`UNMIRRORED_BOOLEAN_FEATURES`] key.
fn is_non_serde_config_path(path: &str) -> bool {
    NON_SERDE_CONFIG_PATHS.contains(&path)
        || crate::util::config::WEB_SEARCH_DOMAIN_CONFIG_PATHS.contains(&path)
        || FEATURES.iter().any(|spec| spec.path == path)
        || path
            .strip_prefix("features.")
            .is_some_and(|key| UNMIRRORED_BOOLEAN_FEATURES.contains(&key))
}
/// Parse `[auth_provider.<name>]` tables leniently: a malformed entry warns
/// (surfaced by `echoagent inspect`) and is skipped, so it fails closed for the
/// models referencing it instead of failing the whole config.
fn parse_auth_providers(
    raw_config: &toml::Value,
) -> (
    IndexMap<String, crate::auth::AuthProviderConfig>,
    Vec<super::config_model_override_parse::ConfigWarning>,
) {
    use super::config_model_override_parse::{ConfigWarning, ConfigWarningKind};
    let mut providers = IndexMap::new();
    let mut warnings = Vec::new();
    let Some(section) = raw_config.get("auth_provider") else {
        return (providers, warnings);
    };
    let Some(table) = section.as_table() else {
        warnings.push(ConfigWarning::auth_provider_section(
            ConfigWarningKind::NotATable,
            format!(
                "`auth_provider` must be a table of [auth_provider.<name>] entries, got {}; \
                 all auth providers ignored",
                section.type_str()
            ),
        ));
        return (providers, warnings);
    };
    for (name, value) in table {
        let mut unknown = Vec::new();
        match serde_ignored::deserialize::<_, _, crate::auth::AuthProviderConfig>(
            value.clone(),
            |path| unknown.push(path.to_string()),
        ) {
            Ok(provider) => {
                for key in unknown {
                    warnings.push(ConfigWarning::auth_provider(
                        name,
                        Some(key.as_str()),
                        ConfigWarningKind::UnknownField,
                        "unrecognized key; field ignored".to_owned(),
                    ));
                }
                for (field, kind, reason) in auth_config_issues(&provider) {
                    warnings.push(ConfigWarning::auth_provider(
                        name,
                        Some(field),
                        kind,
                        reason,
                    ));
                }
                providers.insert(name.clone(), provider);
            }
            Err(error) => {
                warnings.push(ConfigWarning::auth_provider(
                    name,
                    None,
                    ConfigWarningKind::InvalidValue,
                    format!(
                        "failed to parse ({error}); provider skipped, referencing models \
                         resolve with no credential"
                    ),
                ));
            }
        }
    }
    (providers, warnings)
}
impl Config {
    /// Reject invalid glob patterns in the model-filter lists at config load, so
    /// a typo fails loudly instead of silently changing availability.
    pub(crate) fn validate_model_filters(&self) -> Result<(), String> {
        for (field, list) in [
            ("allowed_models", &self.models.allowed_models),
            ("disabled_models", &self.models.disabled_models),
            ("hidden_models", &self.models.hidden_models),
        ] {
            if let Err(bad) = crate::agent::models::ModelGlobSet::compile(list.as_ref()) {
                return Err(format!(
                    "{field} has an invalid pattern: {}. Patterns use * and ? wildcards.",
                    bad.join(", ")
                ));
            }
        }
        Ok(())
    }
    /// Build an `AuthManager` with the configured proxy URL applied.
    pub fn create_auth_manager(&self) -> AuthManager {
        AuthManager::new(
            &crate::util::echo_agent_home::echo_agent_home(),
            self.echo_agent_com_config.clone(),
        )
        .with_proxy_base_url(&self.endpoints.proxy_url())
    }
    /// Deserialize the merged `base` document, also returning the ignored key
    /// paths whose top-level key appears in `user_config`. Paths outside it
    /// can only come from the serialized-defaults half of the merge and must
    /// not be blamed on the user.
    fn deserialize_collecting_unrecognized(
        base: toml::Value,
        user_config: &toml::Value,
    ) -> Result<(Self, Vec<String>), String> {
        let mut unused_keys = Vec::new();
        let config: Self = serde_ignored::deserialize(base, |path| {
            unused_keys.push(path.to_string());
        })
        .map_err(|e| e.to_string())?;
        let entries = &config.features.entries;
        unused_keys.extend(
            entries
                .flags
                .keys()
                .chain(&entries.ignored)
                .map(|key| format!("features.{key}")),
        );
        let unrecognized_keys = match user_config.as_table() {
            Some(user_table) => unused_keys
                .into_iter()
                .filter(|path| {
                    let top_level = path.split('.').next().unwrap_or(path);
                    user_table.contains_key(top_level)
                })
                .filter(|path| !is_non_serde_config_path(path))
                .collect(),
            None => Vec::new(),
        };
        Ok((config, unrecognized_keys))
    }
    pub fn new_from_toml_cfg(raw_config: &toml::Value) -> Result<Self, String> {
        let raw_config = &Self::expand_auth_alias(raw_config);
        let super::config_model_override_parse::ParsedModelOverrides {
            models: config_models,
            warnings: config_warnings,
        } = super::config_model_override_parse::parse_model_overrides(raw_config);
        let (mut auth_providers, auth_provider_warnings) = parse_auth_providers(raw_config);
        let (model_providers, mut model_provider_warnings) = parse_model_providers(raw_config);
        for (id, provider) in &model_providers {
            if let Some(auth) = &provider.auth {
                let synthetic = model_provider_auth_name(id);
                if auth_providers.contains_key(&synthetic) {
                    model_provider_warnings
                        .push(
                            super::config_model_override_parse::ConfigWarning::model_provider(
                                id,
                                Some("auth"),
                                super::config_model_override_parse::ConfigWarningKind::ConflictingFields,
                                format!(
                                "inline auth overwrites a hand-written \
                                 [auth_provider.\"{synthetic}\"]; the `model_provider:` prefix is \
                                 a reserved namespace"
                            ),
                            ),
                        );
                }
                auth_providers.insert(synthetic, auth.clone());
            }
        }
        let mut base = toml::Value::try_from(Self::default()).map_err(|e| e.to_string())?;
        if let toml::Value::Table(ref mut t) = base {
            t.remove("model");
        }
        let mut raw_without_model_sections = raw_config.clone();
        if let toml::Value::Table(ref mut t) = raw_without_model_sections {
            t.remove("model");
            t.remove("auth_provider");
            t.remove("model_providers");
        }
        let parsed_mcp_servers =
            crate::util::config::parse_mcp_servers_from_toml(&raw_without_model_sections);
        if let toml::Value::Table(ref mut t) = raw_without_model_sections {
            t.remove("mcp_servers");
        }
        crate::config::deep_merge_toml(&mut base, &raw_without_model_sections);
        if let toml::Value::Table(ref mut t) = base {
            t.remove("mcp_servers");
        }
        let (mut config, mut unrecognized_keys) =
            Self::deserialize_collecting_unrecognized(base, &raw_without_model_sections)?;
        config.mcp_servers = parsed_mcp_servers.into_iter().collect();
        config.config_models = config_models;
        config.config_warnings = config_warnings;
        config.auth_providers = auth_providers;
        config.model_providers = model_providers;
        for spec in FEATURES {
            let Some(&value) = config.features.entries.flags.get(spec.key) else {
                continue;
            };
            config.feature_values.insert(spec.id, value);
        }
        config.config_warnings.extend(auth_provider_warnings);
        config.config_warnings.extend(model_provider_warnings);
        unrecognized_keys.sort();
        for key in unrecognized_keys {
            config.config_warnings.push(
                super::config_model_override_parse::ConfigWarning::config_key(
                    key,
                    super::config_model_override_parse::ConfigWarningKind::UnknownField,
                    "unrecognized config key".to_owned(),
                ),
            );
        }
        let declared_provider_names: std::collections::HashSet<&str> = raw_config
            .get("auth_provider")
            .and_then(toml::Value::as_table)
            .map(|t| t.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let declared_model_provider_names: std::collections::HashSet<&str> = raw_config
            .get("model_providers")
            .and_then(toml::Value::as_table)
            .map(|t| t.keys().map(String::as_str).collect())
            .unwrap_or_default();
        for (model_key, model) in &config.config_models {
            if let Some(ref name) = model.auth_provider
                && !config.auth_providers.contains_key(name)
                && !declared_provider_names.contains(name.as_str())
            {
                config.config_warnings.push(
                    super::config_model_override_parse::ConfigWarning::model(
                        model_key,
                        Some("auth_provider"),
                        super::config_model_override_parse::ConfigWarningKind::InvalidValue,
                        format!(
                            "references [auth_provider.{name}], which is not defined; \
                             the model resolves with no provider credential"
                        ),
                    ),
                );
            }
            if let Some(ref id) = model.model_provider
                && !config.model_providers.contains_key(id)
                && !declared_model_provider_names.contains(id.as_str())
            {
                config.config_warnings.push(
                    super::config_model_override_parse::ConfigWarning::model(
                        model_key,
                        Some("model_provider"),
                        super::config_model_override_parse::ConfigWarningKind::InvalidValue,
                        format!(
                            "references [model_providers.{id}], which is not defined; \
                             provider defaults are not applied — the model uses its own \
                             credential if set, otherwise fails closed on a custom endpoint"
                        ),
                    ),
                );
            }
        }
        for (id, provider) in &config.model_providers {
            if let Some(ref name) = provider.auth_provider
                && !config.auth_providers.contains_key(name)
                && !declared_provider_names.contains(name.as_str())
            {
                config.config_warnings.push(
                    super::config_model_override_parse::ConfigWarning::model_provider(
                        id,
                        Some("auth_provider"),
                        super::config_model_override_parse::ConfigWarningKind::InvalidValue,
                        format!(
                            "references [auth_provider.{name}], which is not defined; \
                             inheriting models fail closed with no provider credential"
                        ),
                    ),
                );
            }
        }
        if let Some(problem) = config.ui.status_line.problem() {
            config.config_warnings.push(
                super::config_model_override_parse::ConfigWarning::config_key(
                    "ui.status_line".to_owned(),
                    super::config_model_override_parse::ConfigWarningKind::InvalidValue,
                    problem.to_string(),
                ),
            );
        }
        for key in config.ui.status_line.unknown_keys() {
            config.config_warnings.push(
                super::config_model_override_parse::ConfigWarning::config_key(
                    format!("ui.status_line.{key}"),
                    super::config_model_override_parse::ConfigWarningKind::UnknownField,
                    "unrecognized config key".to_owned(),
                ),
            );
        }
        super::config_model_override_parse::log_config_warnings(&config.config_warnings);
        if config.echo_agent_com_config.oidc.is_none() {
            config.echo_agent_com_config.oidc = OidcAuthConfig::from_env();
        }
        if config.echo_agent_com_config.oidc.is_none()
            && config.echo_agent_com_config.oauth2.is_none()
        {
            config.echo_agent_com_config.oauth2 = crate::auth::OAuth2ProviderConfig::from_env();
        }
        if config.client_version.is_none() {
            config.client_version = Self::default().client_version;
        }
        let model_overrides =
            crate::config::ModelOverrideConfig::resolve(None, None, raw_config, None);
        config.web_search_model = model_overrides.web_search;
        config.session_summary_model = model_overrides.session_summary;
        config.image_description_model = model_overrides.image_description;
        config.prompt_suggest_model_pin = model_overrides.prompt_suggestion;
        config.apply_env_overrides();
        Ok(config)
    }
    /// Populate trust-independent `#[serde(skip)]` subagent base fields.
    ///
    /// Must be called after `new_from_toml_cfg` on the **primary startup path**
    /// before the config is handed to `MvpAgent`. Project definitions are overlaid
    /// per cwd after that cwd's authoritative folder-trust resolve.
    pub(crate) fn resolve_subagents(&mut self, cli_flag: bool, raw_config: &toml::Value) {
        let sa = crate::config::SubagentsConfig::resolve(cli_flag, raw_config);
        let remote_settings = self.remote_settings.clone();
        self.resolve_subagent_limits(&sa, remote_settings.as_ref());
        self.subagents_enabled = sa.enabled;
        self.subagent_model_overrides = sa.models;
        self.subagent_toggle = sa.toggle;
        self.subagent_roles = sa.roles;
        self.subagent_personas = sa.personas;
        let env = std::env::var(crate::config::SubagentsConfig::ENV_MAX_DEPTH).ok();
        let remote = self
            .remote_settings
            .as_ref()
            .and_then(|r| r.subagents_max_depth);
        self.subagents_max_depth =
            crate::config::SubagentsConfig::resolve_max_depth(env.as_deref(), sa.max_depth, remote);
    }
    fn resolve_subagent_limits(
        &mut self,
        sa: &crate::config::SubagentsConfig,
        remote: Option<&crate::util::config::RemoteSettings>,
    ) {
        use crate::config::SubagentsConfig;
        let env = |name: &str| std::env::var(name).ok();
        self.subagents_max_concurrent = SubagentsConfig::resolve_max_concurrent(
            env(SubagentsConfig::ENV_MAX_CONCURRENT).as_deref(),
            sa.max_concurrent,
            remote.and_then(|r| r.subagents_max_concurrent),
        );
        self.subagents_sampling_limit = SubagentsConfig::resolve_sampling_limit(
            env(SubagentsConfig::ENV_SAMPLING_LIMIT).as_deref(),
            sa.sampling_limit,
            remote.and_then(|r| r.subagents_sampling_limit),
            self.subagents_max_concurrent,
        );
        self.subagents_limit_behavior = SubagentsConfig::resolve_limit_behavior(
            env(SubagentsConfig::ENV_LIMIT_BEHAVIOR).as_deref(),
            sa.limit_behavior.as_deref(),
            remote.and_then(|r| r.subagents_limit_behavior.as_deref()),
        );
        self.workflow_max_concurrent_agents = SubagentsConfig::resolve_workflow_max_concurrent(
            env(SubagentsConfig::ENV_WORKFLOW_MAX_CONCURRENT).as_deref(),
            sa.workflow_max_concurrent,
            remote.and_then(|r| r.workflow_max_concurrent_agents),
        );
    }
    /// Resolve all `#[serde(skip)]` runtime fields that have resolver functions.
    ///
    /// Call immediately after `new_from_toml_cfg()`. Fields resolved:
    /// - subagents base layers (6 fields) via `SubagentsConfig::resolve`
    /// - respect_gitignore via `ToolsConfig::resolve`
    /// - disable_zdr_incompatible_tools via `ToolsConfig::resolve`
    /// - media_gen_batch_limits via `ToolsConfig::resolve_max_parallel_*`
    /// - managed_mcps_enabled via `ManagedMcpsConfig::resolve`
    /// - web_search_model / session_summary_model / image_description_model /
    ///   prompt_suggest_model_pin via `ModelOverrideConfig::resolve`
    /// - memory_config via typed `Config::resolve_memory`
    /// - disable_web_search (CLI flag ORed with config.toml)
    /// - storage_mode via `StorageMode::resolve`
    /// - path_not_found_hints from remote_settings
    ///
    /// Note: `worktree_type` is resolved directly in `MvpAgent::new` via
    /// `resolve_worktree_type` since it's an agent-level field, not a Config field.
    pub fn resolve_runtime_fields(&mut self, ctx: &RuntimeResolutionContext<'_>) {
        self.cli_subagents = ctx.cli_subagents;
        self.web_search_model_override = ctx.cli_web_search_model.map(|s| s.to_owned());
        self.session_summary_model_override = ctx.cli_session_summary_model.map(|s| s.to_owned());
        let cli_flag = ctx.cli_subagents.unwrap_or(false);
        self.resolve_subagents(cli_flag, ctx.raw_config);
        let env = std::env::var(crate::config::SubagentsConfig::ENV_MAX_DEPTH).ok();
        let toml_max = ctx
            .raw_config
            .get("subagents")
            .and_then(|s| s.get("max_depth"))
            .and_then(|v| v.as_integer());
        let remote = ctx.remote_settings.and_then(|r| r.subagents_max_depth);
        self.subagents_max_depth =
            crate::config::SubagentsConfig::resolve_max_depth(env.as_deref(), toml_max, remote);
        let subagents_toml = crate::config::SubagentsConfig {
            max_concurrent: ctx
                .raw_config
                .get("subagents")
                .and_then(|s| s.get("max_concurrent"))
                .and_then(|v| v.as_integer()),
            limit_behavior: ctx
                .raw_config
                .get("subagents")
                .and_then(|s| s.get("limit_behavior"))
                .and_then(|v| v.as_str())
                .map(str::to_owned),
            workflow_max_concurrent: ctx
                .raw_config
                .get("subagents")
                .and_then(|s| s.get("workflow_max_concurrent"))
                .and_then(|v| v.as_integer()),
            ..Default::default()
        };
        self.resolve_subagent_limits(&subagents_toml, ctx.remote_settings);
        let tools = crate::config::ToolsConfig::resolve(ctx.raw_config);
        self.respect_gitignore = match self.requirements.respect_gitignore.pinned() {
            Some(pinned) => pinned,
            None => tools.respect_gitignore,
        };
        self.disable_zdr_incompatible_tools = tools.disable_zdr_incompatible_tools;
        self.zdr_video_output_s3 = tools.zdr_video_output_s3;
        self.media_gen_batch_limits = echo_agent_tools::media_gen_limits::MediaGenBatchLimits {
            max_image: crate::config::ToolsConfig::resolve_max_parallel_image_gen_calls(
                std::env::var(crate::config::ToolsConfig::ENV_MAX_PARALLEL_IMAGE_GEN_CALLS)
                    .ok()
                    .as_deref(),
                tools.media_gen.max_parallel_image_gen_calls,
                ctx.remote_settings
                    .and_then(|r| r.max_parallel_image_gen_calls),
            ),
            max_video: crate::config::ToolsConfig::resolve_max_parallel_video_gen_calls(
                std::env::var(crate::config::ToolsConfig::ENV_MAX_PARALLEL_VIDEO_GEN_CALLS)
                    .ok()
                    .as_deref(),
                tools.media_gen.max_parallel_video_gen_calls,
                ctx.remote_settings
                    .and_then(|r| r.max_parallel_video_gen_calls),
            ),
        };
        let mcps = crate::config::ManagedMcpsConfig::resolve(
            ctx.raw_config,
            ctx.remote_settings,
            ctx.is_headless,
        );
        self.managed_mcps_enabled = mcps.enabled;
        self.managed_mcp_gateway_tools_enabled = mcps.gateway_tools_enabled;
        let models = crate::config::ModelOverrideConfig::resolve(
            ctx.cli_web_search_model,
            ctx.cli_session_summary_model,
            ctx.raw_config,
            ctx.remote_settings,
        );
        self.web_search_model = models.web_search;
        self.session_summary_model = models.session_summary;
        self.image_description_model = models.image_description;
        self.prompt_suggest_model_pin = models.prompt_suggestion;
        self.memory_enabled_override = ctx.memory_enabled_override;
        let mem = self.resolve_memory(ctx.memory_enabled_override, ctx.remote_settings);
        self.memory_config = if mem.enabled { Some(mem) } else { None };
        self.disable_web_search = self.disable_web_search || ctx.disable_web_search;
        self.todo_gate = ctx.todo_gate;
        self.laziness_debug_log = ctx.laziness_debug_log.map(std::path::Path::to_path_buf);
        self.storage_mode =
            crate::config::StorageMode::resolve(ctx.storage_mode, ctx.remote_settings);
        if let Some(v) = ctx.remote_settings.and_then(|s| s.path_not_found_hints) {
            self.path_not_found_hints = v;
        }
        self.compat_resolved = resolve_compat_config(&self.compat, ctx.remote_settings);
    }
    pub(crate) fn resolve_memory(
        &self,
        memory_enabled_override: Option<bool>,
        remote: Option<&crate::util::config::RemoteSettings>,
    ) -> crate::config::MemoryConfig {
        let default_flush = crate::config::MemoryFlushSettings::default();
        let default_pruning = crate::config::PruningSettings::default();
        crate::config::MemoryConfig::resolve_settings(
            memory_enabled_override,
            &self.memory,
            self.compaction
                .memory_flush
                .as_ref()
                .unwrap_or(&default_flush),
            self.compaction.pruning.as_ref().unwrap_or(&default_pruning),
            remote,
        )
    }
    /// Re-resolve eagerly-resolved runtime fields using the current `Config`
    /// state and fresh `raw_config`. Builds a [`RuntimeResolutionContext`] from
    /// the CLI flags already stored on this `Config`.
    ///
    /// Integration test coverage: `tests/test_settings_refresh.rs`.
    pub(crate) fn re_resolve_runtime_fields(&mut self, raw_config: &toml::Value) {
        match Self::new_from_toml_cfg(raw_config) {
            Ok(parsed_config) => {
                self.memory = parsed_config.memory;
                self.compaction = parsed_config.compaction;
            }
            Err(error) => {
                tracing::warn!(%error, "config parse failed during runtime re-resolution");
            }
        }
        let remote_settings = self.remote_settings.clone();
        let cli_web_search_model = self.web_search_model_override.clone();
        let cli_session_summary_model = self.session_summary_model_override.clone();
        let laziness_debug_log = self.laziness_debug_log.clone();
        let ctx = RuntimeResolutionContext {
            raw_config,
            remote_settings: remote_settings.as_ref(),
            is_headless: self.mode == AgentMode::Headless,
            cli_subagents: self.cli_subagents,
            cli_web_search_model: cli_web_search_model.as_deref(),
            cli_session_summary_model: cli_session_summary_model.as_deref(),
            memory_enabled_override: self.memory_enabled_override,
            disable_web_search: self.disable_web_search,
            todo_gate: self.todo_gate,
            laziness_debug_log: laziness_debug_log.as_deref(),
            storage_mode: None,
        };
        self.resolve_runtime_fields(&ctx);
        crate::util::config::set_remote_campaigns_from_settings(self.remote_settings.as_ref());
    }
    /// If the TOML contains `[auth]`, copy its contents under `[echo_agent_com_config]`.
    /// `[echo_agent_com_config]` takes precedence if both are present (explicit wins).
    ///
    /// This lets customers write the shorter `[auth.oidc]` instead of `[echo_agent_com_config.oidc]`.
    fn expand_auth_alias(raw_config: &toml::Value) -> toml::Value {
        let mut config = raw_config.clone();
        if let toml::Value::Table(ref mut table) = config
            && let Some(auth) = table.remove("auth")
        {
            if let Some(gcc) = table.get_mut("echo_agent_com_config") {
                if let (toml::Value::Table(gcc_table), toml::Value::Table(auth_table)) =
                    (gcc, &auth)
                {
                    for (k, v) in auth_table {
                        gcc_table.entry(k.clone()).or_insert(v.clone());
                    }
                }
            } else {
                table.insert("echo_agent_com_config".to_owned(), auth);
            }
        }
        config
    }
    fn apply_env_overrides(&mut self) {
        self.telemetry.apply_env_overrides();
        if let Some(mode) = env_telemetry_mode("ECHO_AGENT_TELEMETRY_ENABLED") {
            self.features.telemetry = Some(mode);
        }
        self.echo_agent_com_config.force_login_team_uuid = crate::auth::resolve_force_login_team(
            crate::auth::force_login_team_from_requirements(),
            crate::auth::force_login_team_from_env(),
            self.echo_agent_com_config.force_login_team_uuid.take(),
        );
    }
    pub(crate) fn is_telemetry_enabled(&self) -> bool {
        self.resolve_telemetry_mode().value.is_enabled()
    }
    pub fn is_trace_upload_enabled(&self) -> bool {
        self.resolve_trace_upload().value
    }
    pub(crate) fn is_feedback_enabled(&self) -> bool {
        self.is_feature_enabled(Feature::Feedback)
    }
    pub(crate) fn is_session_recap_enabled(&self) -> bool {
        self.is_feature_enabled(Feature::SessionRecap)
    }
    pub(crate) fn is_turn_summary_enabled(&self) -> bool {
        self.is_feature_enabled(Feature::TurnSummary)
    }
    pub(crate) fn is_voice_mode_enabled(&self) -> bool {
        self.is_feature_enabled(Feature::VoiceMode)
    }
    pub(crate) fn is_two_pass_compaction_enabled(&self) -> bool {
        self.is_feature_enabled(Feature::TwoPassCompaction)
    }
    pub(crate) fn resolve_telemetry_mode(&self) -> Resolved<TelemetryMode> {
        if let Some(mode) = self.requirements.telemetry.pinned() {
            return Resolved::new(mode, ConfigSource::Requirement);
        }
        if let Some(mode) = env_telemetry_mode("ECHO_AGENT_TELEMETRY_ENABLED") {
            return Resolved::new(mode, ConfigSource::Env);
        }
        if let Some(mode) = self.features.telemetry {
            return Resolved::new(mode, ConfigSource::Config);
        }
        if let Some(rs) = self.remote_settings.as_ref() {
            if let Some(mode_str) = rs.telemetry_mode.as_deref()
                && let Some(mode) = TelemetryMode::parse(mode_str)
            {
                return Resolved::new(mode, ConfigSource::Remote);
            }
            if let Some(val) = rs.telemetry_enabled {
                return Resolved::new(TelemetryMode::from(val), ConfigSource::Remote);
            }
        }
        Resolved::new(TelemetryMode::Disabled, ConfigSource::Default)
    }
    pub(crate) fn resolve_trace_upload(&self) -> Resolved<bool> {
        let mode = self.resolve_telemetry_mode();
        let ff = if mode.value.is_disabled() {
            None
        } else {
            self.remote_settings
                .as_ref()
                .and_then(|s| s.trace_upload_enabled)
        };
        BoolFlag::env("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD")
            .requirement(self.requirements.trace_upload.pinned())
            .config(self.telemetry.trace_upload)
            .feature_flag(ff)
            .default(mode.value.is_enabled())
            .resolve()
    }
    /// Resolve jemalloc heap-profile config from stored remote settings + gates.
    pub fn resolve_jemalloc_heap_profile(
        &self,
        data_collection_disabled: bool,
    ) -> crate::heap_profile::JemallocHeapProfileConfig {
        let rs = self.remote_settings.as_ref();
        crate::heap_profile::resolve_jemalloc_heap_profile(
            rs.and_then(|s| s.jemalloc_heap_profile_enabled),
            rs.and_then(|s| s.jemalloc_heap_profile_thresholds_bytes.as_deref()),
            rs.and_then(|s| s.jemalloc_heap_profile_poll_interval_secs),
            data_collection_disabled,
            self.resolve_trace_upload().value,
            crate::heap_profile::prof_available(),
        )
    }
    /// K12 scoped resolve: fresh jemalloc fields + current gates (no remote rewrite).
    pub(crate) fn resolve_jemalloc_heap_profile_from_partial(
        &self,
        jemalloc_enabled: Option<bool>,
        jemalloc_thresholds: Option<&[u64]>,
        jemalloc_poll_interval_secs: Option<u64>,
        data_collection_disabled: bool,
    ) -> crate::heap_profile::JemallocHeapProfileConfig {
        crate::heap_profile::resolve_jemalloc_heap_profile(
            jemalloc_enabled,
            jemalloc_thresholds,
            jemalloc_poll_interval_secs,
            data_collection_disabled,
            self.resolve_trace_upload().value,
            crate::heap_profile::prof_available(),
        )
    }
    pub(crate) fn trace_upload_decision_debug(&self) -> serde_json::Value {
        let telemetry = self.resolve_telemetry_mode();
        let trace_upload = self.resolve_trace_upload();
        let req = &self.requirements.trace_upload;
        serde_json::json!({
            "trace_upload": trace_upload.value,
            "trace_upload_source": trace_upload.source.to_string(),
            "telemetry_mode": telemetry.value.to_string(),
            "telemetry_source": telemetry.source.to_string(),
            "in_requirement_pin": req.pinned(),
            "in_requirement_src": req.source().map(|s| s.to_string()),
            "in_env_trace_upload": std::env::var("ECHO_AGENT_TELEMETRY_TRACE_UPLOAD").ok(),
            "in_env_telemetry_enabled": std::env::var("ECHO_AGENT_TELEMETRY_ENABLED").ok(),
            "in_cfg_telemetry_trace_upload": self.telemetry.trace_upload,
            "in_cfg_features_telemetry": self.features.telemetry.map(|m| m.to_string()),
            "in_remote_trace_upload_enabled": self
                .remote_settings
                .as_ref()
                .and_then(|s| s.trace_upload_enabled),
            "has_remote_settings": self.remote_settings.is_some(),
        })
    }
    /// Server-side doom-loop check policy (the `x-echo-agent-doom-loop-check`
    /// header, trigger parsing, and confident-signal resampling, all
    /// applied by the sampler). Merged
    /// PER-FIELD across the `[doom_loop_recovery]` TOML table and the
    /// remote settings `doom_loop_recovery` object (a partial remote object only
    /// overrides the fields it sets). Gate precedence: env
    /// `ECHO_AGENT_DOOM_LOOP_RECOVERY` > TOML `enabled` > remote `enabled` >
    /// default ON — each layer's `false` is an independent kill switch, and
    /// `None` IS the off state, so disabled has exactly one spelling.
    /// Tunables have no env layer (TOML > remote > default) and are clamped
    /// to their documented ranges. Returns the composite runtime policy
    /// rather than `Resolved` because each knob resolves from its own
    /// source (the `resolve_reminder_policy` pattern).
    pub(crate) fn resolve_doom_loop_recovery(
        &self,
    ) -> Option<echo_agent_sampling_types::DoomLoopRecoveryPolicy> {
        use echo_agent_sampling_types::DoomLoopRecoveryPolicy as Policy;
        let remote = self
            .remote_settings
            .as_ref()
            .and_then(|s| s.doom_loop_recovery.as_ref());
        let enabled = BoolFlag::env("ECHO_AGENT_DOOM_LOOP_RECOVERY")
            .config(self.doom_loop_recovery.enabled)
            .feature_flag(remote.and_then(|s| s.enabled))
            .default(true)
            .resolve()
            .value;
        enabled.then(|| Policy {
            max_threshold: self
                .doom_loop_recovery
                .max_threshold
                .or(remote.and_then(|s| s.max_threshold))
                .map_or(Policy::DEFAULT_MAX_THRESHOLD, Policy::clamp_max_threshold),
            max_retries: self
                .doom_loop_recovery
                .max_retries
                .or(remote.and_then(|s| s.max_retries))
                .map_or(Policy::DEFAULT_MAX_RETRIES, Policy::clamp_max_retries),
            window_tokens: self
                .doom_loop_recovery
                .window_tokens
                .or(remote.and_then(|s| s.window_tokens))
                .map_or(
                    Policy::DEFAULT_RECOVERY_WINDOW_TOKENS,
                    Policy::clamp_window_tokens,
                ),
        })
    }
    /// Automatic worktree GC policy. Precedence: env kill/dry-run >
    /// `[worktree.auto_gc]` TOML > remote `worktree_auto_gc` > defaults.
    /// Platform age-expiry (`process_cwd_scan_available`: linux+macos) is enforced
    /// inside `echo_agent_fast_worktree::maybe_auto_gc`, not here.
    pub(crate) fn resolve_worktree_auto_gc(
        &self,
    ) -> echo_agent_fast_worktree::ResolvedWorktreeAutoGc {
        crate::util::config::resolve_worktree_auto_gc_from_settings(
            Some(&self.worktree.auto_gc),
            self.remote_settings
                .as_ref()
                .and_then(|s| s.worktree_auto_gc.as_ref()),
        )
    }
    /// Gate first-run auto-registration of the official EchoAgent marketplace source.
    /// Precedence: env `ECHO_AGENT_OFFICIAL_MARKETPLACE_AUTO_REGISTER` > remote settings >
    /// default off (so only remote settings-targeted teams get it pre-public). No
    /// managed `.requirement` pin: `marketplace_allowlist` already gates sources.
    pub(crate) fn resolve_official_marketplace_auto_register(&self) -> Resolved<bool> {
        let ff = self
            .remote_settings
            .as_ref()
            .and_then(|s| s.official_marketplace_auto_register);
        BoolFlag::env("ECHO_AGENT_OFFICIAL_MARKETPLACE_AUTO_REGISTER")
            .feature_flag(ff)
            .default(false)
            .resolve()
    }
    /// Every tier this config can speak for. A caller with a different remote
    /// snapshot overrides `remote` and leaves the rest.
    pub(crate) fn feature_sources(&self, feature: Feature) -> FeatureSources {
        FeatureSources {
            pin: self.requirements.pinned_feature(feature),
            config: self.feature_values.get(&feature).copied(),
            remote: feature.remote_value(self.remote_settings.as_ref()),
            ..FeatureSources::from_process_env(feature)
        }
    }
    pub fn feature(&self, feature: Feature) -> Resolved<bool> {
        feature.resolve(self.feature_sources(feature))
    }
    pub fn feature_off_reason(&self, feature: Feature) -> Option<String> {
        feature.off_reason(self.feature_sources(feature))
    }
    pub fn is_feature_enabled(&self, feature: Feature) -> bool {
        self.feature(feature).value
    }
    /// Desktop tasks keep the title derived from their opening message. Apply
    /// this at session creation as well as resume: actors reload effective
    /// configuration independently of the launcher's configuration snapshot.
    pub fn is_title_refresh_enabled_for_client(&self, client_identifier: Option<&str>) -> bool {
        client_identifier != Some("echo-agent-desktop") && self.resolve_title_refresh().value
    }
    /// Not a registry row: a row's default is a value, this one is another
    /// feature's answer read at call time. Pinnable anyway, the pin being its own
    /// tier. Unset it follows `turn_summary`; set it to decouple them.
    pub(crate) fn resolve_title_refresh(&self) -> Resolved<bool> {
        let ff = self.remote_settings.as_ref().and_then(|s| s.title_refresh);
        BoolFlag::env("ECHO_AGENT_TITLE_REFRESH")
            .requirement(self.requirements.title_refresh.pinned())
            .config(self.features.title_refresh)
            .feature_flag(ff)
            .default(self.is_feature_enabled(Feature::TurnSummary))
            .resolve()
    }
    /// `image_gen` (+ `/imagine`). Default on.
    ///
    /// `imagine_tools_disabled` is a remote force-off (env/config cannot
    /// re-enable). Otherwise: requirement > env > `[features]` > remote >
    /// default.
    pub(crate) fn resolve_image_gen(&self) -> Resolved<bool> {
        use echo_agent_tools::implementations::echo_agent_build::IMAGE_GEN_TOOL_NAME;
        if let Some(pinned) = self.requirements.image_gen.pinned() {
            return Resolved::new(pinned, ConfigSource::Requirement);
        }
        if self
            .remote_settings
            .as_ref()
            .is_some_and(|s| s.imagine_tool_disabled(IMAGE_GEN_TOOL_NAME))
        {
            return Resolved::new(false, ConfigSource::Remote);
        }
        BoolFlag::env("ECHO_AGENT_IMAGE_GEN")
            .config(self.features.image_gen)
            .feature_flag(
                self.remote_settings
                    .as_ref()
                    .and_then(|s| s.image_gen_enabled),
            )
            .default(true)
            .resolve()
    }
    /// `image_edit` tool gate. Same denylist / requirement pattern as
    /// [`Self::resolve_image_gen`]; no `[features]` key (defaults on).
    pub(crate) fn resolve_image_edit(&self) -> Resolved<bool> {
        use echo_agent_tools::implementations::echo_agent_build::IMAGE_EDIT_TOOL_NAME;
        if let Some(pinned) = self.requirements.image_edit.pinned() {
            return Resolved::new(pinned, ConfigSource::Requirement);
        }
        if self
            .remote_settings
            .as_ref()
            .is_some_and(|s| s.imagine_tool_disabled(IMAGE_EDIT_TOOL_NAME))
        {
            return Resolved::new(false, ConfigSource::Remote);
        }
        BoolFlag::env("ECHO_AGENT_IMAGE_EDIT")
            .default(true)
            .resolve()
    }
    /// `image_to_video` / `reference_to_video` (+ `/imagine-video`). Default on.
    ///
    /// Registered as a pair; denylisting either tool name (or `video_gen`)
    /// disables both. Otherwise same precedence as [`Self::resolve_image_gen`].
    pub(crate) fn resolve_video_gen(&self) -> Resolved<bool> {
        use echo_agent_tools::implementations::echo_agent_build::{
            IMAGE_TO_VIDEO_TOOL_NAME, REFERENCE_TO_VIDEO_TOOL_NAME,
        };
        if let Some(pinned) = self.requirements.video_gen.pinned() {
            return Resolved::new(pinned, ConfigSource::Requirement);
        }
        if self.remote_settings.as_ref().is_some_and(|s| {
            s.imagine_tool_disabled(IMAGE_TO_VIDEO_TOOL_NAME)
                || s.imagine_tool_disabled(REFERENCE_TO_VIDEO_TOOL_NAME)
                || s.imagine_tool_disabled("video_gen")
        }) {
            return Resolved::new(false, ConfigSource::Remote);
        }
        BoolFlag::env("ECHO_AGENT_VIDEO_GEN")
            .config(self.features.video_gen)
            .feature_flag(
                self.remote_settings
                    .as_ref()
                    .and_then(|s| s.video_gen_enabled),
            )
            .default(true)
            .resolve()
    }
    /// Optional Imagine model override for `image_gen`. When set (non-empty),
    /// `image_gen` calls this model slug instead of the default quality model.
    /// Precedence: env `ECHO_AGENT_IMAGE_GEN_MODEL_OVERRIDE` > `[features]
    /// image_gen_model_override` config > remote settings `image_gen_model_override`.
    /// `None` → default model (`echo-agent-imagine-image-quality`).
    pub(crate) fn resolve_image_gen_model_override(&self) -> Option<String> {
        resolve_string_flag(
            None,
            "ECHO_AGENT_IMAGE_GEN_MODEL_OVERRIDE",
            self.features.image_gen_model_override.as_deref(),
            self.remote_settings
                .as_ref()
                .and_then(|s| s.image_gen_model_override.as_deref()),
        )
        .map(|r| r.value)
    }
    pub(crate) fn resolve_image_edit_model_override(&self) -> Option<String> {
        resolve_string_flag(
            None,
            "ECHO_AGENT_IMAGE_EDIT_MODEL_OVERRIDE",
            self.features.image_edit_model_override.as_deref(),
            self.remote_settings
                .as_ref()
                .and_then(|s| s.image_edit_model_override.as_deref()),
        )
        .map(|r| r.value)
    }
    /// Goal mode (`/goal`) master switch. Default ON: deployments that can't
    /// reach cli-chat-proxy `/v1/settings` (custom `models_base_url`, external
    /// `auth_provider_command`, air-gapped proxies) never receive the
    /// remote settings `goal_enabled` flag, so the default must not carve them out.
    pub(crate) fn resolve_goal(&self) -> Resolved<bool> {
        let ff = self.remote_settings.as_ref().and_then(|s| s.goal_enabled);
        if ff == Some(false) {
            return Resolved::new(false, ConfigSource::Remote);
        }
        BoolFlag::env("ECHO_AGENT_GOAL")
            .config(self.goal.enabled)
            .feature_flag(ff)
            .default(true)
            .resolve()
    }
    /// Background workflows (`workflow` tool, `.echo-agent/workflows/*.rhai`,
    /// `/deep-research`, host-owned `/goal` driver). Default ON: deployments
    /// that never receive remote settings still get workflows; `Some(false)`
    /// remote / config / env remains a kill-switch.
    pub(crate) fn resolve_workflows(&self) -> Resolved<bool> {
        let ff = self
            .remote_settings
            .as_ref()
            .and_then(|s| s.workflows_enabled);
        if ff == Some(false) {
            return Resolved::new(false, ConfigSource::Remote);
        }
        BoolFlag::env("ECHO_AGENT_WORKFLOWS")
            .config(self.workflows.enabled)
            .feature_flag(ff)
            .default(true)
            .resolve()
    }
    /// Classifier, planner, and summary all default to goal mode itself: when
    /// `/goal` is on they are on unless config/env/remote says otherwise.
    /// `goal_enabled` is the session's already-resolved master switch (the same
    /// value the actor stores), passed in so a sub-role default can never
    /// disagree with whether `/goal` is on.
    pub(crate) fn resolve_goal_classifier_enabled(&self, goal_enabled: bool) -> Resolved<bool> {
        BoolFlag::env("ECHO_AGENT_GOAL_CLASSIFIER")
            .config(self.goal.classifier_enabled)
            .feature_flag(
                self.remote_settings
                    .as_ref()
                    .and_then(|s| s.goal_classifier_enabled),
            )
            .default(goal_enabled)
            .resolve()
    }
    pub(crate) fn resolve_goal_planner_enabled(&self, goal_enabled: bool) -> Resolved<bool> {
        BoolFlag::env("ECHO_AGENT_GOAL_PLANNER")
            .config(self.goal.planner_enabled)
            .feature_flag(
                self.remote_settings
                    .as_ref()
                    .and_then(|s| s.goal_planner_enabled),
            )
            .default(goal_enabled)
            .resolve()
    }
    pub(crate) fn resolve_goal_summary_enabled(&self, goal_enabled: bool) -> Resolved<bool> {
        BoolFlag::env("ECHO_AGENT_GOAL_SUMMARY")
            .config(self.goal.summary_enabled)
            .feature_flag(
                self.remote_settings
                    .as_ref()
                    .and_then(|s| s.goal_summary_enabled),
            )
            .default(goal_enabled)
            .resolve()
    }
    /// Goal count resolver: env(parse) > config > remote > default, then clamp.
    /// An unparseable env value falls through to the next source.
    fn resolve_goal_u32(
        env_var: &str,
        config: Option<u32>,
        remote: Option<u32>,
        default: u32,
        clamp: impl Fn(u32) -> u32,
    ) -> Resolved<u32> {
        if let Some(env_value) = env_string(env_var)
            && let Ok(parsed) = env_value.parse::<u32>()
        {
            return Resolved::new(clamp(parsed), ConfigSource::Env);
        }
        if let Some(v) = config {
            return Resolved::new(clamp(v), ConfigSource::Config);
        }
        if let Some(v) = remote {
            return Resolved::new(clamp(v), ConfigSource::Remote);
        }
        Resolved::new(default, ConfigSource::Default)
    }
    /// Per-attempt adversarial-skeptic count, clamped to
    /// `[GOAL_VERIFIER_SKEPTIC_MIN, GOAL_VERIFIER_SKEPTIC_MAX]`.
    pub(crate) fn resolve_goal_verifier_count(&self) -> Resolved<u32> {
        use crate::session::goal_classifier::{
            GOAL_VERIFIER_SKEPTIC_COUNT, GOAL_VERIFIER_SKEPTIC_MAX, GOAL_VERIFIER_SKEPTIC_MIN,
        };
        Self::resolve_goal_u32(
            "ECHO_AGENT_GOAL_VERIFIER_N",
            self.goal.verifier_count,
            self.remote_settings
                .as_ref()
                .and_then(|s| s.goal_verifier_count),
            GOAL_VERIFIER_SKEPTIC_COUNT,
            |v| v.clamp(GOAL_VERIFIER_SKEPTIC_MIN, GOAL_VERIFIER_SKEPTIC_MAX),
        )
    }
    /// Per-goal classifier run cap, floored at `GOAL_CLASSIFIER_MAX_RUNS_MIN`
    /// with no upper ceiling.
    pub(crate) fn resolve_goal_classifier_max_runs(&self) -> Resolved<u32> {
        use crate::session::goal_classifier::{
            GOAL_CLASSIFIER_MAX_RUNS_DEFAULT, GOAL_CLASSIFIER_MAX_RUNS_MIN,
        };
        Self::resolve_goal_u32(
            "ECHO_AGENT_GOAL_CLASSIFIER_MAX",
            self.goal.classifier_max_runs,
            self.remote_settings
                .as_ref()
                .and_then(|s| s.goal_classifier_max_runs),
            GOAL_CLASSIFIER_MAX_RUNS_DEFAULT,
            |v| v.max(GOAL_CLASSIFIER_MAX_RUNS_MIN),
        )
    }
    /// Stall-triggered strategist cadence N (fires every N consecutive
    /// `NotAchieved`). Default tracks the resolved classifier cap
    /// (`max(1, cap / 2)`); floored at 1 so it can never silently disable.
    pub(crate) fn resolve_goal_strategist_every(&self, classifier_max_runs: u32) -> Resolved<u32> {
        Self::resolve_goal_u32(
            "ECHO_AGENT_GOAL_STRATEGIST_EVERY",
            self.goal.strategist_every,
            self.remote_settings
                .as_ref()
                .and_then(|s| s.goal_strategist_every),
            (classifier_max_runs / 2).max(1),
            |v| v.max(1),
        )
    }
    /// Re-verify escalation threshold; floored at 1. No remote layer.
    pub(crate) fn resolve_goal_reverify_after(&self) -> Resolved<u32> {
        Self::resolve_goal_u32(
            "ECHO_AGENT_GOAL_REVERIFY_AFTER",
            self.goal.reverify_after,
            None,
            crate::session::acp_session::GOAL_REVERIFY_AFTER_DEFAULT,
            |v| v.max(1),
        )
    }
    /// When `true`, every `/goal` role inherits the current model regardless of
    /// configured pairs.
    pub(crate) fn resolve_goal_use_current_model_only(&self) -> Resolved<bool> {
        BoolFlag::env("ECHO_AGENT_GOAL_USE_CURRENT_MODEL_ONLY")
            .config(self.goal.use_current_model_only)
            .default(false)
            .resolve()
    }
    /// Shared single-pair resolution. Precedence: kill-switch ⇒
    /// `InheritCurrent`/`Config` > `config_pair` ⇒ `Explicit`/`Config` >
    /// `remote_pair` ⇒ `Explicit`/`Remote` > `InheritCurrent`/`Default`. The
    /// chosen pair is cloned only on its branch.
    fn resolve_single_role_model(
        use_current_only: bool,
        config_pair: Option<&crate::util::config::GoalRoleModel>,
        remote_pair: Option<&crate::util::config::GoalRoleModel>,
    ) -> Resolved<GoalRoleModelChoice> {
        if use_current_only {
            return Resolved::new(GoalRoleModelChoice::InheritCurrent, ConfigSource::Config);
        }
        if let Some(pair) = config_pair {
            return Resolved::new(
                GoalRoleModelChoice::Explicit(pair.clone()),
                ConfigSource::Config,
            );
        }
        match remote_pair {
            Some(pair) => Resolved::new(
                GoalRoleModelChoice::Explicit(pair.clone()),
                ConfigSource::Remote,
            ),
            None => Resolved::new(GoalRoleModelChoice::InheritCurrent, ConfigSource::Default),
        }
    }
    /// Planner role model: `[goal]` config then remote. No env layer (only the
    /// kill-switch reads env).
    ///
    /// An `Explicit` pair is applied as `runtime_overrides.model`, resolved before
    /// `resolve_subagent_sampling_config`, so it wins over a user
    /// `[subagents.models]` pin; `InheritCurrent` hands precedence back to that pin.
    pub(crate) fn resolve_goal_planner_model(
        &self,
        use_current_only: bool,
    ) -> Resolved<GoalRoleModelChoice> {
        Self::resolve_single_role_model(
            use_current_only,
            self.goal.planner_model.as_ref(),
            self.remote_settings
                .as_ref()
                .and_then(|s| s.goal_planner_model.as_ref()),
        )
    }
    /// Strategist role model; same precedence as [`Self::resolve_goal_planner_model`].
    pub(crate) fn resolve_goal_strategist_model(
        &self,
        use_current_only: bool,
    ) -> Resolved<GoalRoleModelChoice> {
        Self::resolve_single_role_model(
            use_current_only,
            self.goal.strategist_model.as_ref(),
            self.remote_settings
                .as_ref()
                .and_then(|s| s.goal_strategist_model.as_ref()),
        )
    }
    /// Skeptic pool; same precedence as [`Self::resolve_goal_planner_model`] but
    /// over a pool. Pool order is preserved for the round-robin expansion in
    /// `expand_skeptic_assignment`.
    pub(crate) fn resolve_goal_skeptic_models(
        &self,
        use_current_only: bool,
    ) -> Resolved<Vec<GoalRoleModelChoice>> {
        if use_current_only {
            return Resolved::new(Vec::new(), ConfigSource::Config);
        }
        let to_choices = |pool: &[crate::util::config::GoalRoleModel]| {
            pool.iter()
                .cloned()
                .map(GoalRoleModelChoice::Explicit)
                .collect::<Vec<_>>()
        };
        if !self.goal.skeptic_models.is_empty() {
            return Resolved::new(to_choices(&self.goal.skeptic_models), ConfigSource::Config);
        }
        match self
            .remote_settings
            .as_ref()
            .map(|s| s.goal_skeptic_models.as_slice())
        {
            Some(pool) if !pool.is_empty() => Resolved::new(to_choices(pool), ConfigSource::Remote),
            _ => Resolved::new(Vec::new(), ConfigSource::Default),
        }
    }
    /// Resolve the mode (env `ECHO_AGENT_COMPACTION_MODE` > config > remote settings >
    /// default, unrecognized falling through) and, for `Segments`, attach the
    /// separately-resolved detail level.
    pub(crate) fn resolve_compaction_mode(&self) -> echo_agent_chat_state::CompactionMode {
        resolve_compaction_mode_from(
            env_string("ECHO_AGENT_COMPACTION_MODE").as_deref(),
            self.features.compaction_mode.as_deref(),
            self.remote_settings
                .as_ref()
                .and_then(|r| r.compaction_mode.as_deref()),
        )
        .with_segment_detail(self.resolve_compaction_detail())
    }
    pub(crate) fn resolve_compaction_tool_choice(
        &self,
    ) -> crate::util::config::CompactionToolChoice {
        crate::util::config::resolve_compaction_tool_choice_from(
            env_string(crate::util::config::ENV_COMPACTION_TOOL_CHOICE).as_deref(),
            self.features.compaction_tool_choice.as_deref(),
            self.remote_settings
                .as_ref()
                .and_then(|r| r.compaction_tool_choice.as_deref()),
        )
    }
    /// Precedence: env `ECHO_AGENT_COMPACTION_DETAIL`, then config
    /// `features.compaction_detail`, then remote settings
    /// `remote_settings.compaction_detail`, then default (`verbose`). Drives the
    /// `segments` verbatim detail level.
    fn resolve_compaction_detail(&self) -> echo_agent_chat_state::CompactionDetail {
        resolve_compaction_detail_from(
            env_string("ECHO_AGENT_COMPACTION_DETAIL").as_deref(),
            self.features.compaction_detail.as_deref(),
            self.remote_settings
                .as_ref()
                .and_then(|r| r.compaction_detail.as_deref()),
        )
    }
    /// Resolve whether to use echoagent's default OAuth2 (EchoAgent auth.echo-agent.invalid).
    ///
    /// Enterprise OIDC (`oidc` in config.toml) always wins — this only gates
    /// the default EchoAgent OAuth2 fallback when no enterprise OIDC is configured.
    ///
    /// Priority: `--oauth` > ECHO_AGENT_OAUTH_ENABLED env > default (true = OAuth).
    pub(crate) fn resolve_echo_agent_oauth(&self, cli_oidc: Option<bool>) -> Resolved<bool> {
        BoolFlag::env("ECHO_AGENT_OAUTH_ENABLED")
            .cli(cli_oidc)
            .default(true)
            .resolve()
    }
}
/// Canonical resolver for `mcp.liveness_watchers`. Stacks the full
/// 7-step `BoolFlag` precedence:
///
/// `requirement > cli > env (ECHO_AGENT_MCP_LIVENESS_WATCHERS) > config >
/// managed > feature_flag > default (true)`.
///
/// `util::config::resolve_mcp_liveness_watchers` delegates here so the
/// precedence is single-sourced.
///
/// The default is `true` — it gates the watcher + dispatcher
/// default-on, with this flag existing primarily as a kill switch
/// during the rollout.
pub(crate) fn resolve_mcp_liveness_watchers(
    requirement: Option<bool>,
    cli: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> Resolved<bool> {
    BoolFlag::env("ECHO_AGENT_MCP_LIVENESS_WATCHERS")
        .requirement(requirement)
        .cli(cli)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}
/// Canonical resolver for `mcp.auto_restart`. Stacks the full 7-step
/// `BoolFlag` precedence:
///
/// `requirement > cli > env (ECHO_AGENT_MCP_AUTO_RESTART) > config >
/// managed > feature_flag > default (true)`.
///
/// Mirrors [`resolve_mcp_liveness_watchers`]. Both
/// `util::config::resolve_mcp_auto_restart` delegates here so the
/// precedence is single-sourced.
///
/// Recovery is on by default; opt out via `ECHO_AGENT_MCP_AUTO_RESTART=false`,
/// `[features] mcp_auto_restart`, or `requirements.toml`.
pub(crate) fn resolve_mcp_auto_restart(
    requirement: Option<bool>,
    cli: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> Resolved<bool> {
    BoolFlag::env("ECHO_AGENT_MCP_AUTO_RESTART")
        .requirement(requirement)
        .cli(cli)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}
/// Canonical resolver for `mcp.push_server_status`. Stacks the same
/// 7-step `BoolFlag` precedence as
/// [`resolve_mcp_liveness_watchers`]:
///
/// `requirement > cli > env (ECHO_AGENT_MCP_PUSH_SERVER_STATUS) > config >
/// managed > feature_flag > default (true)`.
///
/// `util::config::resolve_mcp_push_server_status` delegates here so
/// the precedence is single-sourced.
///
/// The default is `true` — the pager's subscription to
/// `echo.agent/mcp/server_status` is wired default-on, with this
/// flag existing primarily as a kill switch.
pub fn resolve_mcp_push_server_status(
    requirement: Option<bool>,
    cli: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> Resolved<bool> {
    BoolFlag::env("ECHO_AGENT_MCP_PUSH_SERVER_STATUS")
        .requirement(requirement)
        .cli(cli)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}
/// Canonical resolver for `mcp.recursive_config_watch`. Stacks the
/// same 7-step `BoolFlag` precedence as
/// [`resolve_mcp_liveness_watchers`]:
///
/// `requirement > cli > env (ECHO_AGENT_MCP_RECURSIVE_CONFIG_WATCH) >
/// config > managed > feature_flag > default (true)`.
///
/// `util::config::resolve_mcp_recursive_config_watch` delegates here
/// so the precedence is single-sourced.
///
/// The default is `true`. It enables the two narrow
/// non-recursive cwd watches default-on. The flag exists primarily
/// as a kill switch during the rollout: if the FSEvents flakiness
/// on macOS or an inotify-quota issue on Linux causes a regression,
/// operators flip this flag (e.g. via `ECHO_AGENT_MCP_RECURSIVE_CONFIG_
/// WATCH=0`) and the leader falls back to the prior behavior (no cwd
/// watches; user-triggered refresh is the only project-config
/// reload path).
///
/// Note the **name is a slight misnomer**: the watches themselves
/// are non-recursive (by design, to avoid blowing through
/// `fs.inotify.max_user_watches` on large repos). The flag name
/// follows the rollout-gate naming convention.
pub(crate) fn resolve_mcp_recursive_config_watch(
    requirement: Option<bool>,
    cli: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> Resolved<bool> {
    BoolFlag::env("ECHO_AGENT_MCP_RECURSIVE_CONFIG_WATCH")
        .requirement(requirement)
        .cli(cli)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}
/// Sync analogue of [`BoolFlag`] for callers that run before the tokio
/// runtime (e.g. `init_sentry`). Loads from disk + env directly rather than
/// from a pre-built `Config`.
///
/// Same convention as [`BoolFlag`]: `resolve()` returns the *enabled* value.
/// `disable_env` is sugar for "force-off if this env is truthy" and does not
/// invert the convention.
///
/// Layer precedence:
/// 1. `requirements.toml`              (admin pin)
/// 2. `managed_settings.json` env      (Claude admin pin, force-off)
/// 3. process env via `disable_env`    (force-off)
/// 4. process env via `enable_env`     (either direction)
/// 5. merged config                    (user/managed defaults)
/// 6. `inherit`, then `default`
pub(crate) struct SyncBoolFlag {
    extract_toml: fn(&toml::Value) -> Option<bool>,
    disable_env: Option<&'static str>,
    enable_env: Option<fn() -> Option<bool>>,
    inherit: Option<fn() -> bool>,
    default: bool,
}
impl SyncBoolFlag {
    pub(crate) const fn new(extract_toml: fn(&toml::Value) -> Option<bool>) -> Self {
        Self {
            extract_toml,
            disable_env: None,
            enable_env: None,
            inherit: None,
            default: false,
        }
    }
    /// Force-off env name (e.g. `"DISABLE_TELEMETRY"`). Truthy at this name
    /// in `managed_settings.json` or process env disables the flag.
    pub(crate) const fn disable_env(mut self, name: &'static str) -> Self {
        self.disable_env = Some(name);
        self
    }
    /// Either-direction env resolver (typically `ECHO_AGENT_*`). Returns
    /// `Some(enabled)` for an explicit signal, `None` to fall through.
    pub(crate) const fn enable_env(mut self, resolver: fn() -> Option<bool>) -> Self {
        self.enable_env = Some(resolver);
        self
    }
    /// Fallback when no source above fires.
    pub(crate) const fn inherit(mut self, resolver: fn() -> bool) -> Self {
        self.inherit = Some(resolver);
        self
    }
    pub(crate) const fn default(mut self, val: bool) -> Self {
        self.default = val;
        self
    }
    pub(crate) fn resolve(&self) -> bool {
        if let Some(enabled) = read_requirements_toml()
            .as_ref()
            .and_then(|r| (self.extract_toml)(r))
        {
            return enabled;
        }
        if let Some(name) = self.disable_env
            && managed_settings_env_flag(name) == Some(true)
        {
            return false;
        }
        if let Some(name) = self.disable_env
            && env_bool(name) == Some(true)
        {
            return false;
        }
        if let Some(resolver) = self.enable_env
            && let Some(enabled) = resolver()
        {
            return enabled;
        }
        if let Some(enabled) = crate::config::load_effective_config()
            .ok()
            .as_ref()
            .and_then(|r| (self.extract_toml)(r))
        {
            return enabled;
        }
        self.inherit.map_or(self.default, |f| f())
    }
}
/// Sync slice of [`Config::resolve_telemetry_mode`] for use before the tokio
/// runtime (e.g. `init_sentry`). `true` only when explicitly off.
pub(crate) fn is_telemetry_disabled_sync() -> bool {
    !SyncBoolFlag::new(telemetry_enabled_from_toml)
        .disable_env("DISABLE_TELEMETRY")
        .enable_env(echo_agent_telemetry_env_enabled)
        .resolve()
}
/// Like [`is_telemetry_disabled_sync`] but only `true` when telemetry is
/// *explicitly* off; absence is not disabled (`.default(true)`) so remote-only
/// enablement still builds the OTLP exporter (the runtime gate then governs it).
pub(crate) fn is_telemetry_explicitly_disabled_sync() -> bool {
    !SyncBoolFlag::new(telemetry_enabled_from_toml)
        .disable_env("DISABLE_TELEMETRY")
        .enable_env(echo_agent_telemetry_env_enabled)
        .default(true)
        .resolve()
}
/// Sync sibling of [`is_telemetry_disabled_sync`] scoped to Sentry. Inherits
/// from telemetry when no Sentry-specific signal is set.
pub fn is_error_reporting_disabled_sync() -> bool {
    !SyncBoolFlag::new(error_reporting_enabled_from_toml)
        .disable_env("DISABLE_ERROR_REPORTING")
        .enable_env(|| env_bool("ECHO_AGENT_ERROR_REPORTING"))
        .inherit(|| !is_telemetry_disabled_sync())
        .resolve()
}
/// `[features] telemetry` as enabled bool. SessionMetrics counts as enabled
/// — see ERROR_REPORTING_PLAN.md. `None` for absent or unparseable.
fn telemetry_enabled_from_toml(root: &toml::Value) -> Option<bool> {
    match root.get("features")?.as_table()?.get("telemetry")? {
        toml::Value::Boolean(b) => Some(*b),
        toml::Value::String(s) => TelemetryMode::parse(s).map(|m| !m.is_disabled()),
        _ => None,
    }
}
/// `[diagnostics] error_reporting` as enabled bool. Bool-only; no
/// `session_metrics` equivalent. `None` falls through to inheritance.
fn error_reporting_enabled_from_toml(root: &toml::Value) -> Option<bool> {
    root.get("diagnostics")?
        .as_table()?
        .get("error_reporting")?
        .as_bool()
}
/// `ECHO_AGENT_TELEMETRY_ENABLED` resolved through `TelemetryMode::parse` so the
/// extended string forms (e.g. `"session_metrics"`) are accepted.
fn echo_agent_telemetry_env_enabled() -> Option<bool> {
    env_telemetry_mode("ECHO_AGENT_TELEMETRY_ENABLED").map(|m| !m.is_disabled())
}
/// Load `~/.echo-agent/requirements.toml` standalone so the admin pin can beat
/// env vars. The merged config layer can't express that — last-merge-wins
/// loses provenance.
pub(crate) fn read_requirements_toml() -> Option<toml::Value> {
    let path = crate::util::echo_agent_home::echo_agent_home().join("requirements.toml");
    let content = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&content).ok()
}
/// Resolve the external-OTEL master switch exactly the way the external
/// stream's activation does: **requirement pin > `ECHO_AGENT_EXTERNAL_OTEL` env >
/// `[telemetry].otel_enabled` config layer (managed config included) > off**.
///
/// The internal trace pipeline keys its "ignore `OTEL_EXPORTER_OTLP_*`"
/// behavior off this value ([`EndpointsConfig::external_otel_master_switch`]),
/// so an org enable distributed via managed config / requirements (no env
/// var) flips **both** sides together. A desync here would leave the
/// internally-authed firehose honoring legacy `OTEL_*` repointing while
/// `internal_pipeline_consumed_otel_vars` simultaneously blocks the external
/// stream — exactly the split this design forbids.
pub(crate) fn external_otel_master_switch_resolved() -> bool {
    external_otel_master_switch_from(
        echo_agent_config::load_merged_requirements().as_ref(),
        env_bool("ECHO_AGENT_EXTERNAL_OTEL"),
        crate::config::load_effective_config().ok().as_ref(),
    )
}
/// Testable core of [`external_otel_master_switch_resolved`].
pub(crate) fn external_otel_master_switch_from(
    requirements: Option<&toml::Value>,
    env_switch: Option<bool>,
    effective_config: Option<&toml::Value>,
) -> bool {
    let table_enabled = |v: Option<&toml::Value>| -> Option<bool> {
        v?.get("telemetry")?.get("otel_enabled")?.as_bool()
    };
    if let Some(pinned) = table_enabled(requirements) {
        return pinned;
    }
    if let Some(env) = env_switch {
        return env;
    }
    table_enabled(effective_config).unwrap_or(false)
}
/// Resolve the external OTEL stream configuration at process startup
/// (env + local config only — remote settings are not yet available when
/// tracing init runs).
///
/// Layering follows `resolve_telemetry_mode`: **requirement > env > config >
/// remote > default**, where the `[telemetry]` `otel_*` keys from the
/// effective config (which already includes managed-config layers distributed
/// by `echoagent setup`) sit under the env vars, requirements pins are applied on
/// top, and the remote layer is restrictive-only + asynchronous
/// ([`apply_external_otel_remote_policy`]).
pub fn resolve_external_otel_config(
    client: echo_agent_telemetry::external::config::ExternalClientInfo,
) -> Option<echo_agent_telemetry::external::ExternalOtelConfig> {
    resolve_external_otel_config_with(
        crate::config::load_effective_config().ok().as_ref(),
        echo_agent_config::load_merged_requirements().as_ref(),
        |name| std::env::var(name).ok(),
        client,
        EndpointsConfig::default().internal_otlp_consumed_standard_vars(),
    )
}
/// Testable core of [`resolve_external_otel_config`]: all inputs injected so
/// tests don't race on process env / disk.
pub(crate) fn resolve_external_otel_config_with(
    effective_config: Option<&toml::Value>,
    requirements: Option<&toml::Value>,
    getenv: impl Fn(&str) -> Option<String>,
    client: echo_agent_telemetry::external::config::ExternalClientInfo,
    internal_pipeline_consumed_otel_vars: bool,
) -> Option<echo_agent_telemetry::external::ExternalOtelConfig> {
    let file_cfg: Option<echo_agent_telemetry::external::ExternalOtelFileConfig> = effective_config
        .and_then(|cfg| cfg.get("telemetry"))
        .map(|t| echo_agent_telemetry::external::ExternalOtelFileConfig {
            enabled: t.get("otel_enabled").and_then(toml::Value::as_bool),
            metrics_exporter: t
                .get("otel_metrics_exporter")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            logs_exporter: t
                .get("otel_logs_exporter")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            endpoint: t
                .get("otel_endpoint")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            protocol: t
                .get("otel_protocol")
                .or_else(|| t.get("otel_transport"))
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            certificate: t
                .get("otel_certificate")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            client_certificate: t
                .get("otel_client_certificate")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            client_key: t
                .get("otel_client_key")
                .and_then(toml::Value::as_str)
                .map(str::to_owned),
            log_user_prompts: t
                .get("otel_log_user_prompts")
                .and_then(toml::Value::as_bool),
            log_tool_details: t
                .get("otel_log_tool_details")
                .and_then(toml::Value::as_bool),
        });
    let req_get =
        |key: &str| -> Option<bool> { requirements?.get("telemetry")?.get(key)?.as_bool() };
    let req_enabled = req_get("otel_enabled");
    let req_prompts = req_get("otel_log_user_prompts");
    let req_details = req_get("otel_log_tool_details");
    let getenv_pinned = |name: &str| -> Option<String> {
        let pin = match name {
            echo_agent_telemetry::external::config::ENV_MASTER_SWITCH => req_enabled,
            "OTEL_LOG_USER_PROMPTS" => req_prompts,
            "OTEL_LOG_TOOL_DETAILS" => req_details,
            _ => None,
        };
        if let Some(v) = pin {
            return Some(if v { "1" } else { "0" }.to_owned());
        }
        getenv(name)
    };
    let mut resolved = echo_agent_telemetry::external::ExternalOtelConfig::resolve_with(
        getenv_pinned,
        file_cfg.as_ref(),
    )?;
    resolved.client = client;
    resolved.internal_pipeline_consumed_otel_vars = internal_pipeline_consumed_otel_vars;
    Some(resolved)
}
/// Apply the restrictive-only remote-settings policy for the external OTEL
/// stream (fleet kill switch + content-gate lock). Tighten-only by
/// construction — there is no remote enable direction — so it is safe to
/// call on every settings refresh.
pub(crate) fn apply_external_otel_remote_policy(
    settings: Option<&crate::util::config::RemoteSettings>,
) {
    let Some(settings) = settings else { return };
    let policy = echo_agent_telemetry::external::ExternalOtelRemotePolicy {
        force_disable: settings.external_otel_disabled.unwrap_or(false),
        lock_content_gates: settings.external_otel_content_gates_locked.unwrap_or(false),
    };
    if policy.force_disable || policy.lock_content_gates {
        echo_agent_telemetry::external::apply_remote_policy(policy);
    }
}
/// Seed free-function remote caches after writing `Config.remote_settings`.
///
/// Called from `init.rs` at boot and from the agent when backgrounded settings
/// arrive later, so every side effect here must be idempotent and safe to
/// re-apply. The emission-gate flip is owned by
/// [`crate::agent::otel_gate::OtelGate`], not here.
///
/// The `force_disable` write here is `Relaxed`; the synchronizing publish is
/// `OtelGate::apply_and_open`, which applies the same tighten-only policy and then
/// opens the gate with a `Release` swap. Removing that second application to
/// deduplicate would leave only the `Relaxed` store and reopen an ARM
/// visibility hole.
pub fn apply_remote_settings_side_effects(settings: Option<&crate::util::config::RemoteSettings>) {
    if let Some(s) = settings {
        let origin_trusted = crate::util::is_prod_cli_chat_proxy_url(
            &EndpointsConfig::from_effective_config().proxy_url(),
        );
        echo_agent_config::signed_policy::apply_remote_managed_config_signature_verification(
            s.managed_config_signature_verification,
            origin_trusted,
        );
    }
    crate::util::config::cache_remote_mcp_startup_timeout_secs(
        settings.and_then(|s| s.mcp_startup_timeout_secs),
    );
    crate::util::config::cache_remote_max_mcp_output_bytes(
        settings.and_then(|s| s.max_mcp_output_bytes),
    );
    crate::util::config::cache_remote_auto_mode(settings.and_then(|s| s.auto_mode.clone()));
    crate::util::config::cache_remote_remember_tool_approvals(
        settings.and_then(|s| s.remember_tool_approvals),
    );
    crate::util::config::cache_remote_crash_handler_enabled(
        settings.and_then(|s| s.crash_handler_enabled),
    );
    apply_external_otel_remote_policy(settings);
    let image_normalize_cache_enabled = settings
        .and_then(|r| r.image_normalize_cache_enabled)
        .unwrap_or(false);
    crate::session::normalize_cache::NormalizeCache::global()
        .set_enabled(image_normalize_cache_enabled);
}
/// Read `env.<key>` from Claude-compat `managed_settings.json`. `Some(true)`
/// indicates a force-off signal from a Mac-MDM-style admin policy.
fn managed_settings_env_flag(key: &str) -> Option<bool> {
    let path = echo_agent_config::claude_managed_settings_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&content).ok()?;
    echo_agent_workspace::permission::resolution::json_env_flag(json.get("env"), key)
}
/// Assemble the final model map. Priority (highest wins):
/// config.toml `[model.*]` > prefetched (remote) > hardcoded defaults.
pub(crate) fn resolve_model_list(
    cfg: &Config,
    prefetched: Option<IndexMap<String, ModelEntry>>,
) -> IndexMap<String, ModelEntry> {
    let mut resolved: IndexMap<String, ModelEntry> = IndexMap::new();
    if cfg.endpoints.has_custom_endpoint() {
        tracing::info!(
            models_base_url = ?cfg.endpoints.models_base_url,
            models_list_url = ?cfg.endpoints.models_list_url,
            "custom models endpoint active, skipping built-in defaults",
        );
    } else {
        let defaults = default_model_entries(&cfg.endpoints);
        tracing::debug!(count = defaults.len(), "loaded default models");
        resolved.extend(defaults);
    }
    if let Some(mut prefetched) = prefetched {
        tracing::debug!(count = prefetched.len(), "loaded prefetched models");
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        for (key, entry) in prefetched.iter_mut() {
            let donor = resolved.get(key);
            if let Some(donor) = donor {
                if entry.info.context_window.get() == default_cw
                    && donor.info.context_window.get() != default_cw
                {
                    tracing::debug!(
                        model_key = %key,
                        model = %entry.info.model,
                        client_default = default_cw,
                        inherited = donor.info.context_window.get(),
                        donor_model = %donor.info.model,
                        "prefetched model missing context_window, inheriting from hardcoded default"
                    );
                    entry.info.context_window = donor.info.context_window;
                }
                if entry.info.agent_type == DEFAULT_AGENT_TYPE {
                    entry.info.agent_type.clone_from(&donor.info.agent_type);
                }
                if entry.info.api_backend == ApiBackend::default() {
                    entry.info.api_backend.clone_from(&donor.info.api_backend);
                }
            }
            if resolved.contains_key(key) {
                tracing::debug!(model_key = %key, "prefetched model overriding default");
            }
        }
        resolved = prefetched;
    }
    for (key, model_override) in &cfg.config_models {
        let had_base = resolved.contains_key(key);
        let base = resolved.shift_remove(key);
        if !had_base {
            tracing::debug!(model_key = %key, "config model adding new entry (not in defaults/prefetched)");
            if model_override.context_window.is_none() {
                tracing::debug!(
                    model_key = %key,
                    default = 200_000,
                    "new model missing context_window, defaulting to 200000 — set context_window in [model.{}] to override",
                    key,
                );
            }
        }
        let with_provider = model_override.model_provider.as_deref().map(|pid| {
            match cfg.model_providers.get(pid) {
                Some(provider) => model_override.with_provider_defaults(provider, pid),
                None => model_override.with_missing_provider(),
            }
        });
        let effective = with_provider.as_ref().unwrap_or(model_override);
        let mut entry = effective.apply(key, base, &cfg.endpoints);
        let session_bearer_unsafe =
            !crate::util::is_echo_agent_api_bearer_url(&entry.info.base_url)
                || entry
                    .api_base_url
                    .as_deref()
                    .is_some_and(|url| !crate::util::is_echo_agent_api_bearer_url(url));
        if let Some(pid) = model_override.model_provider.as_deref()
            && entry.auth_provider.is_none()
            && session_bearer_unsafe
            && !is_desktop_keyless_endpoint(&entry.info.base_url)
        {
            entry.auth_provider = Some(crate::auth::AuthProviderRef::fail_closed(format!(
                "model_provider:{pid} (fail-closed)"
            )));
        }
        tracing::debug!(
            model_key = %key,
            base_url = %entry.info.base_url,
            has_api_key = entry.api_key.is_some(),
            env_key = ?entry.env_key,
            auth_provider = entry.auth_provider.as_ref().map(|p| p.name.as_str()),
            model_provider = model_override.model_provider.as_deref(),
            had_base,
            "config model override applied"
        );
        resolved.insert(key.clone(), entry);
    }
    for (key, entry) in resolved.iter_mut() {
        if let Some(ref mut provider) = entry.auth_provider {
            if provider.is_fail_closed() {
                continue;
            }
            let config = cfg.auth_providers.get(&provider.name);
            if config.is_none() {
                tracing::debug!(
                    model_key = %key,
                    provider = %provider.name,
                    "provider ref has no trusted config; failing closed with an empty command"
                );
            }
            provider.attach_trusted_config(config);
        }
    }
    {
        let default_cw = DEFAULT_CONTEXT_WINDOW;
        let donors: std::collections::HashMap<String, (std::num::NonZeroU64, ApiBackend)> =
            resolved
                .values()
                .filter(|e| e.info.context_window.get() != default_cw)
                .map(|e| {
                    (
                        e.info.model.clone(),
                        (e.info.context_window, e.info.api_backend.clone()),
                    )
                })
                .collect();
        for entry in resolved.values_mut() {
            if let Some((donor_cw, donor_backend)) = donors.get(&entry.info.model) {
                if entry.info.context_window.get() == default_cw {
                    tracing::debug!(
                        model = %entry.info.model,
                        from = default_cw,
                        to = donor_cw.get(),
                        "slug-match: inheriting context_window from sibling catalog entry"
                    );
                    entry.info.context_window = *donor_cw;
                }
                if entry.info.api_backend == ApiBackend::default()
                    && *donor_backend != ApiBackend::default()
                {
                    entry.info.api_backend.clone_from(donor_backend);
                }
            }
        }
    }
    if let Some(ref global_agent_type) = cfg.models.agent_type {
        tracing::warn!(
            global_agent_type = %global_agent_type,
            "[models] agent_type is deprecated. Set agent_type on each [model.X] entry instead."
        );
        for entry in resolved.values_mut() {
            if entry.info.agent_type == DEFAULT_AGENT_TYPE {
                entry.info.agent_type = global_agent_type.clone();
            }
        }
    }
    apply_global_extra_headers(&mut resolved, &cfg.models);
    apply_global_scalar_defaults(&mut resolved, &cfg.models);
    for entry in resolved.values_mut() {
        entry.info.derive_reasoning_effort_fields();
    }
    resolved
}
/// Layer 6 of [`resolve_model_list`]: fold the global `[models].extra_headers`
/// into every model as a base. The presence check is case-insensitive because
/// the sampler lowers these into an `http::HeaderMap`, so a global `X-Foo` must
/// not shadow a per-model `x-foo`; a per-model `[model.<id>].extra_headers`
/// (applied earlier) therefore wins per key.
fn apply_global_extra_headers(resolved: &mut IndexMap<String, ModelEntry>, models: &ModelsConfig) {
    if models.extra_headers.is_empty() {
        return;
    }
    tracing::debug!(
        header_keys = ?models.extra_headers.keys().collect::<Vec<_>>(),
        model_count = resolved.len(),
        "applying global [models].extra_headers default to all models"
    );
    for entry in resolved.values_mut() {
        for (k, v) in &models.extra_headers {
            let present = entry
                .info
                .extra_headers
                .keys()
                .any(|ek| ek.eq_ignore_ascii_case(k));
            if !present {
                entry.info.extra_headers.insert(k.clone(), v.clone());
            }
        }
    }
}
/// Layer 7 of [`resolve_model_list`]: fill scalar `[models]` defaults into any
/// model that left the field unset. Per-model (Layer 3) and remote-prefetched
/// (Layer 2) values already populated theirs, so they win via `get_or_insert`
/// (the global default is a fallback, not a clamp).
fn apply_global_scalar_defaults(
    resolved: &mut IndexMap<String, ModelEntry>,
    models: &ModelsConfig,
) {
    for entry in resolved.values_mut() {
        let info = &mut entry.info;
        if let Some(v) = models.temperature {
            info.temperature.get_or_insert(v);
        }
        if let Some(v) = models.top_p {
            info.top_p.get_or_insert(v);
        }
        if let Some(v) = models.max_completion_tokens {
            info.max_completion_tokens.get_or_insert(v);
        }
        if let Some(v) = models.max_retries {
            info.max_retries.get_or_insert(v);
        }
        if let Some(v) = models.inference_idle_timeout_secs {
            info.inference_idle_timeout_secs.get_or_insert(v);
        }
        if let Some(v) = models.subagent_rate_limit_max_attempts {
            info.subagent_rate_limit_max_attempts.get_or_insert(v);
        }
        if let Some(v) = models.stream_tool_calls {
            info.stream_tool_calls.get_or_insert(v);
        }
    }
}
/// Built-in default models. Prefer `resolve_model_list()`.
pub(crate) fn default_model_entries(endpoints: &EndpointsConfig) -> IndexMap<String, ModelEntry> {
    default_models(endpoints)
        .into_iter()
        .map(|(key, entry)| (key, ModelEntry::from_config_entry(&entry)))
        .collect()
}
/// Resolve a model against the available model map.
/// Checks the map key (id) first, then falls back to a slug scan.
pub(crate) fn find_model_by_id<'a>(
    models: &'a IndexMap<String, ModelEntry>,
    model_id: &str,
) -> Option<&'a ModelEntry> {
    models
        .get(model_id)
        .or_else(|| models.values().find(|m| m.model == model_id))
}
/// Whether the EFFECTIVE Auto-mode classifier model supports reasoning effort:
/// the model actually routed to (`aux_model` when the aux sampler resolved) else
/// the session model the worker falls back to. Not-found-in-catalog ⇒ `false`
/// (conservative; also covers the Tier-2 synthetic proxy entry). Drives the
/// built-in `low` effort default.
pub(crate) fn effective_classifier_supports_re(
    aux_model: Option<&str>,
    session_model: &str,
    models: &IndexMap<String, ModelEntry>,
) -> bool {
    find_model_by_id(models, aux_model.unwrap_or(session_model))
        .map(|e| e.info().supports_reasoning_effort)
        .unwrap_or(false)
}
/// JSON-only subset of `ModelEntryConfig`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DefaultModelJson {
    id: Option<String>,
    model: String,
    model_family: Option<String>,
    name: Option<String>,
    description: Option<String>,
    context_window: Option<NonZeroU64>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    max_completion_tokens: Option<u32>,
    api_backend: ApiBackend,
    #[serde(default = "default_agent_type")]
    agent_type: String,
    inference_idle_timeout_secs: Option<u64>,
    hidden: bool,
    reasoning_effort: Option<ReasoningEffort>,
    #[serde(default)]
    supports_reasoning_effort: bool,
    #[serde(default)]
    reasoning_efforts: Vec<ReasoningEffortOption>,
    /// When false, only OAuth users see this in the picker.
    #[serde(default = "default_true")]
    supported_in_api: bool,
    #[serde(default)]
    supports_backend_search: bool,
    #[serde(default)]
    compactions_remaining: Option<CompactionsRemaining>,
    #[serde(default)]
    compaction_at_tokens: Option<CompactionAtTokens>,
    #[serde(default)]
    show_model_fingerprint: bool,
    #[serde(default)]
    auto_compact_threshold_percent: Option<u8>,
    #[serde(default)]
    system_prompt_label: Option<String>,
}
fn default_models(endpoints: &EndpointsConfig) -> IndexMap<String, ModelEntryConfig> {
    let root: serde_json::Value = serde_json::from_str(crate::models::DEFAULT_MODELS_JSON)
        .expect("default_models.json: invalid JSON");
    let entries: Vec<DefaultModelJson> = serde_json::from_value(
        root.get("models")
            .expect("default_models.json: missing 'models' array")
            .clone(),
    )
    .expect("default_models.json: invalid 'models' array");
    tracing::debug!(
        count = entries.len(),
        "loaded default models from embedded JSON"
    );
    entries
        .into_iter()
        .map(|m| {
            assert!(
                !m.model.is_empty(),
                "default_models.json: entry id={:?} has empty `model` field",
                m.id
            );
            let key = m.id.clone().unwrap_or_else(|| m.model.clone());
            let context_window = m
                .context_window
                .unwrap_or_else(|| NonZeroU64::new(200_000).expect("200000 is non-zero"));
            let config = ModelEntryConfig {
                id: m.id,
                model: m.model,
                model_family: m.model_family,
                base_url: endpoints.resolve_inference_base_url(),
                api_base_url: Some(endpoints.echo_agent_api_base_url.clone()),
                name: m.name,
                description: m.description,
                context_window,
                auto_compact_threshold_percent: m.auto_compact_threshold_percent,
                system_prompt_label: m.system_prompt_label,
                temperature: m.temperature,
                top_p: m.top_p,
                max_completion_tokens: m.max_completion_tokens,
                api_backend: m.api_backend,
                auth_scheme: None,
                agent_type: m.agent_type,
                inference_idle_timeout_secs: m.inference_idle_timeout_secs,
                max_retries: None,
                subagent_rate_limit_max_attempts: None,
                api_key: None,
                env_key: None,
                extra_headers: IndexMap::new(),
                use_concise: false,
                hidden: m.hidden,
                supported_in_api: m.supported_in_api,
                reasoning_effort: m.reasoning_effort,
                supports_reasoning_effort: m.supports_reasoning_effort,
                reasoning_efforts: m.reasoning_efforts,
                supports_backend_search: m.supports_backend_search,
                compactions_remaining: m.compactions_remaining,
                compaction_at_tokens: m.compaction_at_tokens,
                show_model_fingerprint: m.show_model_fingerprint,
                stream_tool_calls: None,
                laziness_detector: LazinessDetectorPerModelConfig::default(),
            };
            (key, config)
        })
        .collect()
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntryConfig {
    /// Stable unique identifier for this catalog entry. When present,
    /// used as the catalog map key. Falls back to `model` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The routing slug sent in API requests.
    pub model: String,
    /// See [`ModelInfo::model_family`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,
    /// The base URL of the model. e.g. "https://api.echo-agent.invalid/v1"
    pub base_url: String,
    /// Human-readable display name of the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// The API key for this model's provider.
    /// If not set, falls back to env_key, then ECHO_AGENT_API_KEY.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Environment variable name(s) that hold the provider API key.
    /// Accepts a string or an array (first set, non-empty value wins).
    /// If not set, falls back to ECHO_AGENT_API_KEY.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env_key: Option<EnvKeys>,
    /// Which API backend to use for this model.
    /// Values: "chat_completions" (default), "responses"
    #[serde(default)]
    pub api_backend: ApiBackend,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_scheme: Option<AuthScheme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_reasoning_effort: bool,
    /// Per-model reasoning-effort menu (source of truth). The two legacy fields
    /// above are derived from this list when it is non-empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<ReasoningEffortOption>,
    /// Extra headers to send with requests to this model's endpoint.
    /// Useful for BYOK (Bring Your Own Key) scenarios.
    /// Example: { "x-anthropic-api-key" = "sk-ant-..." }
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub extra_headers: IndexMap<String, String>,
    /// The total context window size in tokens for this model.
    /// Used for auto-compact threshold calculations.
    /// Required — BYOK users must explicitly set this in config.toml.
    pub context_window: NonZeroU64,
    /// Per-model auto-compact threshold (0-100). When the session's token
    /// usage exceeds this percentage of `context_window`, the conversation
    /// is summarized. Resolver precedence:
    /// requirements > env > user (per-model > global) > managed (per-model > global)
    /// > remote per-model (this field) > remote global > 85.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold_percent: Option<u8>,
    /// Per-model system-prompt identity label (not UI `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_label: Option<String>,
    /// The base URL to use when authenticating with an API key (non-session auth).
    /// When set, `base_url` is used for session-based auth and `api_base_url` for API key auth.
    /// When not set, `base_url` is used for all auth methods (e.g. BYOK / third-party models).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    /// When true, this model uses concise mode (compact system prompt,
    /// concise tool output, concise user message prefix, reduced toolset).
    /// Defaults to false — when omitted or false, nothing changes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub use_concise: bool,
    /// The type of system prompt to use for this model.
    /// e.g. "echo-agent-build", "codex".
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// Maximum seconds to wait between SSE chunks during inference streaming.
    /// When no chunk is received within this duration, the request fails with
    /// a non-retryable `IdleTimeout` error. This is a per-chunk deadline that
    /// resets on every received chunk — NOT a total-turn timeout.
    /// Default: 300 seconds (5 minutes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_idle_timeout_secs: Option<u64>,
    /// Maximum number of retries for transient API errors (429, 500, 502, etc.)
    /// during a single inference request. Default: 5.
    /// Can also be set via the `ECHO_AGENT_MAX_RETRIES` environment variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_rate_limit_max_attempts: Option<u32>,
    /// Exclude from the client model picker; still usable internally (web_search, etc.).
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// When false, only OAuth users see this in the picker.
    #[serde(default = "default_true")]
    pub supported_in_api: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub supports_backend_search: bool,
    /// Per-model config for the `x-compactions-remaining` header; `None` disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compactions_remaining: Option<CompactionsRemaining>,
    /// Per-model config for the `x-compaction-at` header; `None` disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_at_tokens: Option<CompactionAtTokens>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub show_model_fingerprint: bool,
    /// Inject `stream_tool_calls: true` into the request body
    /// so the upstream emits per-chunk `function_call_arguments.delta`
    /// Without this set, EchoAgent API models send args as one delta
    /// event, defeating the purpose of streaming.
    ///
    /// Per-model opt-in -- BYOK endpoints that don't understand the
    /// flag should leave this unset to avoid request errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_tool_calls: Option<bool>,
    /// Per-model Layer-3 LazinessDetector configuration. Defaults to
    /// the all-disabled state via `#[serde(default)]`.
    #[serde(default, skip_serializing_if = "is_default_laziness_detector")]
    pub laziness_detector: LazinessDetectorPerModelConfig,
}
/// True when `cfg` equals the all-disabled default. Derives `PartialEq`
/// on `f32`, which is fine for the current shape because both `f32`
/// fields default to `None` — there's no parsed-vs-literal `0.7` float
/// equality footgun. If a future default introduces `Some(0.7)`, this
/// helper must be reworked (e.g. compare on tolerance, or switch to a
/// bit-pattern compare) so `skip_serializing_if` doesn't start emitting
/// `[laziness_detector]` blocks for every model in `config.toml`.
fn is_default_laziness_detector(cfg: &LazinessDetectorPerModelConfig) -> bool {
    cfg == &LazinessDetectorPerModelConfig::default()
}
/// A `[model.foo]` entry from config.toml, parsed directly from raw TOML
/// (bypassing deep merge). Scalar fields are `Option` so absent means "inherit
/// from defaults/prefetched"; the collection fields (`extra_headers`,
/// `reasoning_efforts`) merge only when non-empty and so cannot express
/// "override to empty."
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ConfigModelOverride {
    pub model: Option<String>,
    pub model_family: Option<String>,
    pub base_url: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub api_key: Option<String>,
    /// Env var name(s) for the provider key — string or array in config.toml.
    pub env_key: Option<EnvKeys>,
    /// Name of a `[auth_provider.<name>]` credential helper that mints
    /// this model's bearer token. Static `api_key` / `env_key` win when both
    /// are set.
    pub auth_provider: Option<String>,
    pub model_provider: Option<String>,
    pub api_base_url: Option<String>,
    pub max_completion_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub api_backend: Option<ApiBackend>,
    pub auth_scheme: Option<AuthScheme>,
    #[serde(default)]
    pub extra_headers: IndexMap<String, String>,
    #[serde(default)]
    pub query_params: IndexMap<String, String>,
    #[serde(default)]
    pub env_http_headers: IndexMap<String, String>,
    pub context_window: Option<u64>,
    /// Per-model auto-compact threshold override (0-100) from `[model.<id>]`.
    /// Read directly by `resolve_auto_compact_threshold_percent`; intentionally
    /// NOT merged into `ModelInfo.auto_compact_threshold_percent` so the
    /// resolver can keep user-per-model distinct from GB-per-model.
    pub auto_compact_threshold_percent: Option<u8>,
    /// Per-model system-prompt identity; not merged into `ModelInfo` (tiered resolve).
    pub system_prompt_label: Option<String>,
    pub use_concise: Option<bool>,
    pub agent_type: Option<String>,
    pub inference_idle_timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    pub subagent_rate_limit_max_attempts: Option<u32>,
    pub hidden: Option<bool>,
    pub supported_in_api: Option<bool>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub supports_reasoning_effort: Option<bool>,
    pub reasoning_efforts: Vec<ReasoningEffortOption>,
    pub supports_backend_search: Option<bool>,
    /// Aliases must be registered in `config_model_override_parse::ALIASES`;
    /// serde rejects a table that contains both spellings otherwise.
    #[serde(alias = "send_compactions_remaining")]
    pub compactions_remaining: Option<CompactionsRemaining>,
    pub compaction_at_tokens: Option<CompactionAtTokens>,
    pub show_model_fingerprint: Option<bool>,
    pub stream_tool_calls: Option<bool>,
}
impl ConfigModelOverride {
    pub(crate) fn apply(
        &self,
        key: &str,
        base: Option<ModelEntry>,
        endpoints: &EndpointsConfig,
    ) -> ModelEntry {
        let mut entry = base.unwrap_or_else(|| ModelEntry::fallback(key, endpoints));
        if let Some(ref v) = self.model {
            entry.info.model = v.clone();
        }
        if self.model_family.is_some() {
            entry.info.model_family.clone_from(&self.model_family);
        }
        if let Some(ref v) = self.base_url {
            entry.info.base_url = v.clone();
            if self.api_base_url.is_none() {
                entry.api_base_url = None;
            }
        }
        if self.name.is_some() {
            entry.info.name.clone_from(&self.name);
        }
        if self.description.is_some() {
            entry.info.description.clone_from(&self.description);
        }
        if self.max_completion_tokens.is_some() {
            entry.info.max_completion_tokens = self.max_completion_tokens;
        }
        if self.temperature.is_some() {
            entry.info.temperature = self.temperature;
        }
        if self.top_p.is_some() {
            entry.info.top_p = self.top_p;
        }
        if let Some(ref v) = self.api_backend {
            entry.info.api_backend = v.clone();
        }
        if let Some(v) = self.auth_scheme {
            entry.info.auth_scheme = v;
        }
        if !self.extra_headers.is_empty() {
            entry.info.extra_headers = self.extra_headers.clone();
        }
        if !self.query_params.is_empty() {
            entry.info.query_params = self.query_params.clone();
        }
        if !self.env_http_headers.is_empty() {
            entry.info.env_http_headers = self.env_http_headers.clone();
        }
        if let Some(cw) = self.context_window.and_then(NonZeroU64::new) {
            entry.info.context_window = cw;
        }
        if let Some(v) = self.use_concise {
            entry.info.use_concise = v;
        }
        if let Some(ref at) = self.agent_type {
            entry.info.agent_type.clone_from(at);
        }
        if self.inference_idle_timeout_secs.is_some() {
            entry.info.inference_idle_timeout_secs = self.inference_idle_timeout_secs;
        }
        if self.max_retries.is_some() {
            entry.info.max_retries = self.max_retries;
        }
        if self.subagent_rate_limit_max_attempts.is_some() {
            entry.info.subagent_rate_limit_max_attempts = self.subagent_rate_limit_max_attempts;
        }
        if let Some(v) = self.hidden {
            entry.info.hidden = v;
        }
        if let Some(v) = self.supported_in_api {
            entry.info.supported_in_api = v;
        }
        if self.reasoning_effort.is_some() {
            entry.info.reasoning_effort = self.reasoning_effort;
        }
        if let Some(v) = self.supports_reasoning_effort {
            entry.info.supports_reasoning_effort = v;
        } else if !entry.info.supports_reasoning_effort
            && matches!(entry.info.api_backend, ApiBackend::Messages)
        {
            entry.info.supports_reasoning_effort = true;
        }
        if !self.reasoning_efforts.is_empty() {
            entry.info.reasoning_efforts = self.reasoning_efforts.clone();
        }
        if let Some(v) = self.supports_backend_search {
            entry.info.supports_backend_search = v;
        }
        if self.compactions_remaining.is_some() {
            entry.info.compactions_remaining = self.compactions_remaining;
        }
        if self.compaction_at_tokens.is_some() {
            entry.info.compaction_at_tokens = self.compaction_at_tokens;
        }
        if let Some(v) = self.show_model_fingerprint {
            entry.info.show_model_fingerprint = v;
        }
        if self.stream_tool_calls.is_some() {
            entry.info.stream_tool_calls = self.stream_tool_calls;
        }
        if self.api_key.is_some() {
            entry.api_key.clone_from(&self.api_key);
        }
        if self.env_key.is_some() {
            entry.env_key.clone_from(&self.env_key);
        }
        if let Some(ref name) = self.auth_provider {
            entry.auth_provider = Some(crate::auth::AuthProviderRef::unresolved(name.clone()));
        }
        if self.api_base_url.is_some() {
            entry.api_base_url.clone_from(&self.api_base_url);
        }
        if self.supported_in_api.is_none()
            && (self.api_key.is_some() || self.env_key.is_some() || self.auth_provider.is_some())
        {
            entry.info.supported_in_api = true;
        }
        entry
    }
}
/// Shared model metadata — the common fields across all model sources.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelInfo {
    /// Stable unique identifier for this catalog entry.
    /// Falls back to `model` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The routing slug sent in API requests.
    pub model: String,
    /// Provider family that mints this model's conversation items
    /// (e.g. "echoagent"); `None` = unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_family: Option<String>,
    /// The base URL of the model (session endpoint). e.g. "https://chat-proxy.echo-agent.invalid/v1"
    pub base_url: String,
    /// Human-readable name of the model. Honored by both the picker
    /// (`/model`) and `/session-info` -- when set, that's the label shown
    /// to users in either consumer.
    pub name: Option<String>,
    pub description: Option<String>,
    pub max_completion_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub api_backend: ApiBackend,
    pub auth_scheme: AuthScheme,
    pub extra_headers: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub query_params: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
    pub env_http_headers: IndexMap<String, String>,
    pub context_window: NonZeroU64,
    /// Per-model auto-compact threshold (0-100). `None` defers to the
    /// global / default tiers in `resolve_auto_compact_threshold_percent`.
    pub auto_compact_threshold_percent: Option<u8>,
    /// Per-model system-prompt identity (not UI picker `name`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_label: Option<String>,
    /// When true, this model uses concise mode (compact system prompt,
    /// concise tool output, concise user message prefix, reduced toolset).
    pub use_concise: bool,
    /// The type of agent configuration to use for this model.
    /// Always has a value; defaults to `"echo-agent-build-plan"` when the server
    /// or user config doesn't specify one.
    #[serde(default = "default_agent_type")]
    pub agent_type: String,
    /// Per-chunk idle timeout for inference streaming (see `ModelEntryConfig`).
    pub inference_idle_timeout_secs: Option<u64>,
    pub max_retries: Option<u32>,
    pub subagent_rate_limit_max_attempts: Option<u32>,
    /// Never show in picker (any auth). See also `supported_in_api`.
    pub hidden: bool,
    /// May the user select this model for normal chat? Derived from
    /// `allowed_models` in `resolve_model_catalog`; never persisted.
    #[serde(skip_serializing, default = "default_true")]
    pub user_selectable: bool,
    /// When false, only OAuth users see this in the picker.
    #[serde(default = "default_true")]
    pub supported_in_api: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// When true, the UI shows effort controls for this model.
    pub supports_reasoning_effort: bool,
    /// Per-model reasoning-effort menu (source of truth); legacy fields derived from it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_efforts: Vec<ReasoningEffortOption>,
    pub supports_backend_search: bool,
    /// Per-model config for the `x-compactions-remaining` header; `None` disables it.
    pub compactions_remaining: Option<CompactionsRemaining>,
    /// Per-model config for the `x-compaction-at` header; `None` disables it.
    pub compaction_at_tokens: Option<CompactionAtTokens>,
    pub show_model_fingerprint: bool,
    /// When `Some(true)`, the sampler injects `stream_tool_calls: true`
    pub stream_tool_calls: Option<bool>,
    /// Per-model Layer-3 LazinessDetector configuration. Defaults to
    /// the all-disabled state — the feature is per-model opt-in with a
    /// second-step `max_nudges_per_session > 0` opt-in for actually
    /// injecting nudges. See [`LazinessDetectorPerModelConfig`].
    #[serde(default)]
    pub laziness_detector: LazinessDetectorPerModelConfig,
}
impl ModelInfo {
    /// Minimal fallback descriptor for an unknown model slug.
    /// Used when a configured model ID isn't found in presets or remote models.
    pub fn fallback(slug: &str) -> Self {
        ModelInfo {
            user_selectable: true,
            id: None,
            model: slug.to_owned(),
            model_family: None,
            base_url: String::new(),
            name: None,
            description: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: ApiBackend::default(),
            auth_scheme: Default::default(),
            extra_headers: IndexMap::new(),
            query_params: IndexMap::new(),
            env_http_headers: IndexMap::new(),
            context_window: NonZeroU64::new(200_000).unwrap(),
            auto_compact_threshold_percent: None,
            system_prompt_label: None,
            use_concise: false,
            agent_type: default_agent_type(),
            inference_idle_timeout_secs: None,
            max_retries: None,
            subagent_rate_limit_max_attempts: None,
            hidden: false,
            supported_in_api: true,
            reasoning_effort: None,
            supports_reasoning_effort: false,
            reasoning_efforts: Vec::new(),
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            show_model_fingerprint: false,
            stream_tool_calls: None,
            laziness_detector: LazinessDetectorPerModelConfig::default(),
        }
    }
    /// Extract shared model metadata from a flat config entry.
    pub(crate) fn from_config(entry: &ModelEntryConfig) -> Self {
        ModelInfo {
            user_selectable: true,
            id: entry.id.clone(),
            model: entry.model.clone(),
            model_family: entry.model_family.clone(),
            base_url: entry.base_url.clone(),
            name: entry.name.clone(),
            description: entry.description.clone(),
            max_completion_tokens: entry.max_completion_tokens,
            temperature: entry.temperature,
            top_p: entry.top_p,
            api_backend: entry.api_backend.clone(),
            auth_scheme: entry.auth_scheme.unwrap_or_default(),
            extra_headers: entry.extra_headers.clone(),
            query_params: IndexMap::new(),
            env_http_headers: IndexMap::new(),
            context_window: entry.context_window,
            auto_compact_threshold_percent: entry.auto_compact_threshold_percent,
            system_prompt_label: entry.system_prompt_label.clone(),
            use_concise: entry.use_concise,
            agent_type: entry.agent_type.clone(),
            inference_idle_timeout_secs: entry.inference_idle_timeout_secs,
            max_retries: entry.max_retries,
            subagent_rate_limit_max_attempts: entry.subagent_rate_limit_max_attempts,
            hidden: entry.hidden,
            supported_in_api: entry.supported_in_api,
            reasoning_effort: entry.reasoning_effort,
            supports_reasoning_effort: entry.supports_reasoning_effort,
            reasoning_efforts: entry.reasoning_efforts.clone(),
            supports_backend_search: entry.supports_backend_search,
            compactions_remaining: entry.compactions_remaining,
            compaction_at_tokens: entry.compaction_at_tokens,
            show_model_fingerprint: entry.show_model_fingerprint,
            stream_tool_calls: entry.stream_tool_calls,
            laziness_detector: entry.laziness_detector.clone(),
        }
    }
    /// Derive the legacy effort gate/default from `reasoning_efforts` so the
    /// shell's internal reads (support gate, wire default, session modes) treat
    /// a menu-only model as supported. The single derive site; `to_acp_model_info`
    /// then just reads these fields. Idempotent (the remote/CCP path already sets
    /// them); the empty-list path leaves both legacy fields untouched.
    fn derive_reasoning_effort_fields(&mut self) {
        if self.reasoning_efforts.is_empty() {
            return;
        }
        self.supports_reasoning_effort = true;
        if self.reasoning_effort.is_none() {
            let default = self
                .reasoning_efforts
                .iter()
                .find(|opt| opt.default)
                .or_else(|| self.reasoning_efforts.first())
                .map(|opt| opt.value);
            self.reasoning_effort = default;
        }
    }
    /// Whether this model appears in the picker for the given auth mode.
    ///
    /// | `hidden` | `supported_in_api` | OAuth user | API-key user |
    /// |----------|--------------------|------------|--------------|
    /// | true     | _                  | hidden     | hidden       |
    /// | false    | true               | visible    | visible      |
    /// | false    | false              | visible    | **hidden**   |
    pub(crate) fn visible_for_auth(&self, is_session_auth: bool) -> bool {
        !self.hidden && (is_session_auth || self.supported_in_api)
    }
}
/// Flat struct so credential and endpoint fields coexist after deep-merge.
/// Routing reads fields, not provenance.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ModelEntry {
    pub info: ModelInfo,
    pub api_key: Option<String>,
    pub env_key: Option<EnvKeys>,
    /// Named credential helper (`[model.<id>] auth_provider = "<name>"`),
    /// resolved against `[auth_provider.<name>]` by `resolve_model_list`.
    /// Config-file models only: the built-in catalog never carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_provider: Option<crate::auth::AuthProviderRef>,
    /// When set, `base_url` is used for session auth, `api_base_url` for API-key auth.
    pub api_base_url: Option<String>,
}
impl ModelEntry {
    /// Minimal fallback entry for an unknown model slug.
    pub fn fallback(slug: &str, endpoints: &EndpointsConfig) -> Self {
        let mut info = ModelInfo::fallback(slug);
        info.base_url = endpoints.resolve_inference_base_url();
        Self {
            info,
            api_key: None,
            env_key: None,
            auth_provider: None,
            api_base_url: None,
        }
    }
    pub fn info(&self) -> &ModelInfo {
        &self.info
    }
    pub(crate) fn from_config_entry(entry: &ModelEntryConfig) -> Self {
        Self {
            info: ModelInfo::from_config(entry),
            api_key: entry.api_key.clone(),
            env_key: entry.env_key.clone(),
            auth_provider: None,
            api_base_url: entry.api_base_url.clone(),
        }
    }
    /// Non-empty `api_key`, else first non-empty resolved `env_key`.
    /// `None` → fall through to session / global key. Static only: never
    /// consults auth-provider tokens.
    pub(crate) fn own_credential(&self) -> Option<String> {
        first_own_credential(self.api_key.as_deref(), self.env_key.as_ref())
    }
    /// The provider governing this model's bearer: `None` when a static
    /// `api_key`/`env_key` resolves. The turn paths consult this, so a
    /// shadowed provider never runs.
    pub(crate) fn effective_auth_provider(&self) -> Option<&crate::auth::AuthProviderRef> {
        if self.own_credential().is_some() {
            return None;
        }
        self.auth_provider.as_ref()
    }
    /// `true` when the model has a credential source or uses the desktop's
    /// explicitly approved keyless endpoint.
    /// Probes `std::env::var` at call time: result is not stable across env
    /// changes. Never executes a provider command.
    pub(crate) fn has_own_credentials(&self) -> bool {
        self.own_credential().is_some()
            || self.auth_provider.is_some()
            || is_desktop_keyless_endpoint(&self.info.base_url)
    }
}

fn is_desktop_keyless_endpoint(base_url: &str) -> bool {
    base_url.trim_end_matches('/') == "http://123.56.188.16:8088/v1"
}
impl std::ops::Deref for ModelEntry {
    type Target = ModelInfo;
    fn deref(&self) -> &ModelInfo {
        &self.info
    }
}
fn is_false(v: &bool) -> bool {
    !v
}
fn default_true() -> bool {
    true
}
/// Codebase indexing setting for `[features] codebase_indexing`.
///
/// Patterns are matched against the git root when available, otherwise the cwd,
/// which allows explicitly indexing non-git directories.
///
/// ```toml
/// codebase_indexing = false                                          # disable
/// codebase_indexing = true                                           # any git repo (default)
/// codebase_indexing = ["/Users/*/echoagent*", "!/Users/*/old-*"]           # globs, ! to exclude
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CodebaseIndexingSetting {
    Enabled(bool),
    Patterns(Vec<String>),
}
impl Default for CodebaseIndexingSetting {
    fn default() -> Self {
        Self::Enabled(true)
    }
}
impl CodebaseIndexingSetting {
    /// Should `path` be indexed? For `Enabled(true)`, always yes (caller gates on git-root).
    /// For `Patterns`, path must match an include and not match any `!exclude`.
    pub(crate) fn should_index(&self, path: &std::path::Path) -> bool {
        match self {
            Self::Enabled(b) => *b,
            Self::Patterns(patterns) => {
                let path_str = path.to_string_lossy();
                let matches_any = |pats: &[&str]| {
                    pats.iter()
                        .any(|p| glob::Pattern::new(p).is_ok_and(|pat| pat.matches(&path_str)))
                };
                let (excludes, includes): (Vec<_>, Vec<_>) =
                    patterns.iter().partition(|p| p.starts_with('!'));
                let excludes: Vec<&str> = excludes
                    .iter()
                    .map(|p| p.strip_prefix('!').unwrap_or(p.as_str()))
                    .collect();
                let includes: Vec<&str> = includes.iter().map(|p| p.as_str()).collect();
                let included = includes.is_empty() || matches_any(&includes);
                let excluded = matches_any(&excludes);
                included && !excluded
            }
        }
    }
}
/// Optional role pair that drops a malformed value to `None` (with a warn)
/// instead of failing the whole config parse — one typo must not wipe the
/// config. Mirrors the remote tolerance in `util::config::remote`.
fn de_tolerant_goal_role_model<'de, D>(
    deserializer: D,
) -> Result<Option<crate::util::config::GoalRoleModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    Ok(value.and_then(|v| {
        v.try_into()
            .map_err(|e| tracing::warn!(error = %e, "[goal] role model: dropped malformed value"))
            .ok()
    }))
}
/// Skeptic pool variant of [`de_tolerant_goal_role_model`]: a non-array yields
/// an empty pool; malformed entries are dropped, survivor order preserved (the
/// skeptic round-robin depends on it).
fn de_tolerant_goal_role_models<'de, D>(
    deserializer: D,
) -> Result<Vec<crate::util::config::GoalRoleModel>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<toml::Value>::deserialize(deserializer)?;
    Ok(match value {
        Some(toml::Value::Array(arr)) => arr
            .into_iter()
            .filter_map(|v| {
                v.try_into()
                    .map_err(|e| {
                        tracing::warn!(error = %e, "[goal] skeptic model: dropped malformed entry");
                    })
                    .ok()
            })
            .collect(),
        _ => Vec::new(),
    })
}
/// `[goal]` section: the canonical home for `/goal` configuration. Field names
/// mirror the remote `goal_*` keys with the prefix dropped, so config and remote
/// stay 1:1. Per-key precedence is env > this config > remote > default.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct GoalConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub planner_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub use_current_model_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_max_runs: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategist_every: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reverify_after: Option<u32>,
    #[serde(
        default,
        deserialize_with = "de_tolerant_goal_role_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub planner_model: Option<crate::util::config::GoalRoleModel>,
    #[serde(
        default,
        deserialize_with = "de_tolerant_goal_role_model",
        skip_serializing_if = "Option::is_none"
    )]
    pub strategist_model: Option<crate::util::config::GoalRoleModel>,
    #[serde(
        default,
        deserialize_with = "de_tolerant_goal_role_models",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub skeptic_models: Vec<crate::util::config::GoalRoleModel>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkflowsConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}
/// `[auto_mode]` section: server-side configuration for Auto permission mode.
/// ONE struct serves both the local `[auto_mode]` TOML table and the remote
/// remote settings `auto_mode` JSON object (coerced via `serde_json::from_value`), so
/// the two stay 1:1. All fields are plain scalars/enums, so they deserialize
/// cleanly from both formats (no custom tolerant deser needed). Unset fields stay
/// `None` here; the wire fn applies the built-in defaults once auto mode is
/// enabled (current model, `low` effort if the model supports it, `just_command`
/// prompt). Precedence: local config > remote > those built-in defaults.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoModeConfig {
    /// The Auto-mode gate. Lowest-precedence layer of the gate chain (env and
    /// local `[auto_mode] enabled` config win over this remote value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// How much context the classifier prompt includes. `None` ⇒ the wire fn's
    /// built-in default (`just_command`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_type: Option<echo_agent_workspace::permission::ClassifierPromptType>,
    /// Routing slug for a dedicated classifier model. `None` ⇒ inherit the
    /// session model. Resolved via `resolve_aux_model_sampling_config`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classifier_model: Option<String>,
    /// Classifier side-query duration in milliseconds; resolved with bounded defaults.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classify_timeout_ms: Option<u64>,
    /// Classifier reasoning effort. Applies on BOTH the routed-model path and the
    /// inherited session-model path; `None` ⇒ the wire fn's built-in default
    /// (`low` if the effective model supports reasoning effort, else unset).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Features {
    /// when set, the agent may ask permission for tool executions
    #[serde(default)]
    pub support_permission: bool,
    /// `None` = defer to remote settings / default (off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetryMode>,
    /// Codebase graph indexing for go-to-definition/references.
    /// Accepts: true | false | ["glob", "!negative-glob", ...]
    /// Default: true (index any git repo). Patterns can explicitly match non-git directories.
    #[serde(default)]
    pub codebase_indexing: CodebaseIndexingSetting,
    /// Show a blocking warning when EchoAgent starts outside a Git repository.
    /// Default: false. Used as the local fallback when the `non_git_warning` remote settings
    /// flag in `echo_agent_build_settings` is absent. When the remote flag is present it takes
    /// precedence — `Some(false)` from remote settings overrides `true` here.
    #[serde(default)]
    pub non_git_warning: bool,
    /// Managed config fetching (managed_config.toml + requirements.toml).
    /// `None` = defer to env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_config: Option<bool>,
    /// Early-session auto-title refresh. `None` = defer to `resolve_title_refresh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title_refresh: Option<bool>,
    /// `image_gen` / `/imagine`. `None` = env / remote / default (`true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_gen: Option<bool>,
    /// Video tools / `/imagine-video`. `None` = env / remote / default (`true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_gen: Option<bool>,
    /// `image_gen` Imagine model override. `None`/empty = defer to remote settings
    /// (`image_gen_model_override`) / env / default (`echo-agent-imagine-image-quality`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_gen_model_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_edit_model_override: Option<String>,
    /// `summary` (default) | `transcript` | `segments`. `None` = defer to CLI /
    /// env (`ECHO_AGENT_COMPACTION_MODE`). Parsed via `CompactionMode::parse`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_mode: Option<String>,
    /// `none` | `minimal` | `balanced` | `verbose` (default). `None` = defer to
    /// env (`ECHO_AGENT_COMPACTION_DETAIL`). The `segments` verbatim detail level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_tool_choice: Option<String>,
    /// Per-`Ready`-client transport-liveness pollers + the
    /// session-actor `StatusDispatcher`.
    ///
    /// When `true` (default), each successfully-handshaken MCP
    /// client gets a poller that detects rmcp service-loop
    /// termination and pushes `echo.agent/mcp/server_status` updates to
    /// the client. When `false`, neither watchers nor the
    /// dispatcher are spawned — useful as an emergency kill switch
    /// for the rollout. `None` = defer to env / default (true).
    ///
    /// Not read through this struct: the live resolver re-reads the
    /// `[features]` key out-of-band from raw TOML in
    /// `util::config::resolve::mcp`. Declared so `serde_ignored`
    /// does not report it as an unrecognized key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_liveness_watchers: Option<bool>,
    /// Bounded stdio auto-restart task.
    ///
    /// When `true`, the session-actor `StatusDispatcher` reacts to
    /// `TransportClosed` / `HandshakeFailed` events on stdio MCP
    /// servers by scheduling up to 3 respawn attempts with
    /// `[1s, 4s, 16s]` backoff. HTTP / HttpAuth servers are NOT
    /// auto-restarted (their existing `reset_transport` path
    /// covers the recovery). `None` = defer to env / default
    /// (recovery is on by default; set `false` here / via
    /// `ECHO_AGENT_MCP_AUTO_RESTART` to opt out).
    ///
    /// Not read through this struct: the live resolver re-reads the
    /// `[features]` key out-of-band from raw TOML in
    /// `util::config::resolve::mcp`. Declared so `serde_ignored`
    /// does not report it as an unrecognized key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_auto_restart: Option<bool>,
    /// Pager-side subscription to the `echo.agent/mcp/server_status` push.
    ///
    /// When `true` (default), the pager subscribes to the per-server
    /// status delta the shell emits via the dispatcher and
    /// patches the MCP servers modal in-place (no re-fetch round
    /// trip). When `false`, the pager ignores the push and falls
    /// back to the legacy `echo.agent/mcp/tools_changed` debounced refetch
    /// path. `None` = defer to env / default (true).
    ///
    /// Not read through this struct. The pager-side gate
    /// (`acp_handler::push_server_status_enabled`) uses an
    /// **env-only** OnceLock cache via
    /// [`crate::util::config::resolve_mcp_push_server_status(None, None, None)`],
    /// which consults `BoolFlag::env` and the default `true`. The
    /// `[features]` key itself is honoured out-of-band, re-read from
    /// raw TOML in `util::config::resolve::mcp`. This field is
    /// declared so `serde_ignored` does not report the key as
    /// unrecognized.
    ///
    /// Practical consequence: setting
    /// `[features] mcp_push_server_status = false` in
    /// `~/.echo-agent/config.toml` will NOT disable the pager's
    /// subscription on a freshly-launched process. To disable the
    /// pager subscription, set `ECHO_AGENT_MCP_PUSH_SERVER_STATUS=0` in
    /// the env before launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_push_server_status: Option<bool>,
    /// Whether the leader's `ConfigFileWatcher` adds the two narrow
    /// non-recursive watches for `<cwd>/` and `<cwd>/.echo-agent/`.
    ///
    /// When `true` (default), edits to `<cwd>/.mcp.json`,
    /// `<cwd>/.echo-agent/config.toml`, or `<cwd>/.claude.json` flow
    /// through the watcher → reloader → `ConfigUpdate::
    /// ProjectMcpServersChanged { cwd }` → `app.rs` ACP-injection
    /// pipeline and the affected sessions reload their MCP servers
    /// within the debounce window (~ 1 s). When `false`, the leader
    /// skips the cwd watches entirely and the only way to pick up a
    /// project-config edit is the user-triggered refresh button.
    ///
    /// The watches are **always non-recursive** — the name follows
    /// the convention for the rollout-gate flag. See
    /// `crate::config::watcher::ConfigFileWatcher::watch_path` for
    /// the inotify-quota rationale.
    ///
    /// The name is a documented misnomer — it gates
    /// the existence of the **cwd** watches, NOT their recursion
    /// mode. A future rename to `mcp_cwd_config_watch` would align
    /// name and behavior; deferred to a follow-up to avoid widening
    /// the config surface across requirements.toml / managed configs.
    ///
    /// Not read through this struct: the live resolver re-reads the
    /// `[features]` key out-of-band from raw TOML in
    /// `util::config::resolve::mcp`. Declared so `serde_ignored`
    /// does not report it as an unrecognized key.
    /// `None` = defer to env / default (true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_recursive_config_watch: Option<bool>,
    /// Every remaining `[features]` key, typed. Private: a registry key must be
    /// read through [`Config::feature`], which resolves the other tiers too.
    #[serde(flatten)]
    entries: FeatureEntries,
}
/// The `[features]` entries no field claims: the registry rows, the keys the
/// raw-layer resolvers read, and whatever a typo or a later release leaves
/// behind. Typing them here is what checks a key before anyone thinks to list
/// it, which is the whole point: a quoted `remote_fetch` once read as absent
/// and left an egress gate open.
///
/// Deserialized by hand for two reasons serde cannot cover: to name the key,
/// which its message for a bad map value omits, and to fail only on a value
/// that reads as a boolean, so a key holding a later release's typed value is
/// ignored rather than fatal to a build that predates the field.
#[derive(Clone, Debug, Default)]
struct FeatureEntries {
    flags: BTreeMap<String, bool>,
    /// Keys holding neither a boolean nor a mistaken one, kept so the operator
    /// still hears about them.
    ignored: std::collections::BTreeSet<String>,
}
impl Serialize for FeatureEntries {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.flags.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for FeatureEntries {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntriesVisitor;
        impl<'de> serde::de::Visitor<'de> for EntriesVisitor {
            type Value = FeatureEntries;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a table of `[features]` booleans")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut flags = BTreeMap::new();
                let mut ignored = std::collections::BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    let value: toml::Value = map.next_value()?;
                    match value.as_bool() {
                        Some(flag) => {
                            flags.insert(key, flag);
                        }
                        None if reads_as_a_boolean(&value) => {
                            return Err(serde::de::Error::custom(non_boolean_feature_error(
                                &format!("features.{key}"),
                                &value,
                            )));
                        }
                        None => {
                            ignored.insert(key);
                        }
                    }
                }
                Ok(FeatureEntries { flags, ignored })
            }
        }
        deserializer.deserialize_map(EntriesVisitor)
    }
}
/// Resolved credentials for a model session.
pub(crate) struct ResolvedCredentials {
    pub api_key: Option<String>,
    pub base_url: String,
    pub auth_type: echo_agent_chat_state::AuthType,
    pub auth_scheme: AuthScheme,
}
/// First usable BYOK credential: a non-empty (trimmed) api_key, else the first
/// set, non-empty env_key value. Single source of truth for has_own_credentials,
/// resolve_credentials, and the JWT-reload path.
pub(crate) fn first_own_credential(
    api_key: Option<&str>,
    env_key: Option<&EnvKeys>,
) -> Option<String> {
    api_key
        .filter(|k| !k.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| env_key.and_then(EnvKeys::resolve_value))
}
/// Priority: model api_key/env_key > cached auth-provider token > approved
/// keyless endpoint > session token > ECHO_AGENT_API_KEY.
pub(crate) fn resolve_credentials(
    model: &ModelEntry,
    session_key: Option<&str>,
) -> ResolvedCredentials {
    let info = model.info();
    let (api_key, base_url, auth_type) = if let Some(key) = model.own_credential() {
        (
            Some(key),
            info.base_url.clone(),
            echo_agent_chat_state::AuthType::ApiKey,
        )
    } else if let Some(provider) = model.auth_provider.as_ref() {
        debug_assert!(model.effective_auth_provider().is_some());
        (
            provider.cached_token(),
            info.base_url.clone(),
            echo_agent_chat_state::AuthType::ApiKey,
        )
    } else if is_desktop_keyless_endpoint(&info.base_url) {
        // Never forward a session or global first-party token to this public endpoint.
        (
            None,
            info.base_url.clone(),
            echo_agent_chat_state::AuthType::ApiKey,
        )
    } else if let Some(key) = session_key {
        (
            Some(key.to_owned()),
            info.base_url.clone(),
            echo_agent_chat_state::AuthType::SessionToken,
        )
    } else if let Ok(key) = crate::agent::auth_method::read_echo_agent_api_key_env() {
        let url = model
            .api_base_url
            .clone()
            .unwrap_or_else(|| info.base_url.clone());
        (Some(key), url, echo_agent_chat_state::AuthType::ApiKey)
    } else {
        if let Some(ref env_keys) = model.env_key
            && !env_keys.is_empty()
        {
            tracing::warn!(
                model = %info.model,
                env_key = %env_keys,
                "model has env_key configured but none of the environment variables are set — \
                 requests will have no API key",
            );
        }
        (
            None,
            info.base_url.clone(),
            echo_agent_chat_state::AuthType::ApiKey,
        )
    };
    let auth_scheme = info.auth_scheme;
    tracing::debug!(
        model = %info.model,
        auth_type = ?auth_type,
        "resolved credentials"
    );
    ResolvedCredentials {
        api_key,
        base_url,
        auth_type,
        auth_scheme,
    }
}
/// `disable_api_key_auth` at the credential seam: swap a first-party EchoAgent API
/// key for the IdP session (absent => request fails => forces login). BYOK
/// (non-EchoAgent `base_url`) is untouched; no-op when the switch is off.
pub(crate) fn enforce_disable_api_key_auth(
    creds: &mut ResolvedCredentials,
    disable_api_key_auth: bool,
    session_key: Option<&str>,
) {
    if disable_api_key_auth
        && creds.auth_type == echo_agent_chat_state::AuthType::ApiKey
        && crate::util::is_echo_agent_api_url(&creds.base_url)
    {
        creds.auth_type = echo_agent_chat_state::AuthType::SessionToken;
        creds.api_key = session_key.map(str::to_owned);
        echo_agent_telemetry::unified_log::debug(
            "auth: kill switch blocked a first-party API key at the credential seam",
            None,
            Some(serde_json::json!({
                "replaced_with_session": session_key.is_some(),
                "base_url": creds.base_url,
            })),
        );
    }
}
/// Resolve credentials for an auxiliary sampling path (web search, image
/// description) with the first-party API-key kill switch applied, so these
/// paths honor `disable_api_key_auth` exactly like the main chat path.
fn resolve_credentials_enforced(
    entry: &ModelEntry,
    session_key: Option<&str>,
    disable_api_key_auth: bool,
) -> ResolvedCredentials {
    let mut credentials = resolve_credentials(entry, session_key);
    enforce_disable_api_key_auth(&mut credentials, disable_api_key_auth, session_key);
    credentials
}
pub use echo_agent_telemetry::config::deployment_id_from_key;
/// Try to resolve credentials for a model by loading the effective config.
/// Returns `None` (with a warning) if config loading, parsing, or model
/// lookup fails. `session_key` should only be passed when `auth_type` is
/// `SessionToken` — callers must guard this.
pub(crate) fn try_resolve_model_credentials(
    model_id: &str,
    session_key: Option<&str>,
) -> Option<ResolvedCredentials> {
    let raw = crate::config::load_effective_config()
        .map_err(|e| tracing::warn!(error = %e, "config load failed for credential resolution"))
        .ok()?;
    let cfg = Config::new_from_toml_cfg(&raw)
        .map_err(|e| tracing::warn!(error = %e, "config parse failed for credential resolution"))
        .ok()?;
    let models = resolve_model_list(&cfg, None);
    let entry = find_model_by_id(&models, model_id)?;
    let mut credentials = resolve_credentials(entry, session_key);
    enforce_disable_api_key_auth(
        &mut credentials,
        cfg.echo_agent_com_config.api_key_auth_disabled(),
        session_key,
    );
    Some(credentials)
}
/// Per-model auth facts (BYOK status + auth scheme) from one effective-config
/// load, memoized by the session actor.
#[derive(Clone, Copy)]
pub(crate) struct ModelAuthFacts {
    pub byok: ModelByok,
    pub auth_scheme: AuthScheme,
}
/// Resolve `model_id` to its auth facts and auth-provider reference from one
/// effective-config load; both ride the same memo (see
/// `SessionActor::model_auth_memo`). Load/parse failure → `byok = Unknown`;
/// model absent from the catalog → `NotByok`. An empty `model_id` (no sampling
/// config yet) → `Unknown`, not `NotByok`, so the gate isn't activated for an
/// unidentified model.
pub(crate) fn resolve_model_auth_facts_and_provider(
    model_id: &str,
) -> (ModelAuthFacts, Option<crate::auth::AuthProviderRef>) {
    if model_id.is_empty() {
        return (
            ModelAuthFacts {
                byok: ModelByok::Unknown,
                auth_scheme: AuthScheme::default(),
            },
            None,
        );
    }
    with_resolved_model(model_id, |lookup| {
        let facts = ModelAuthFacts {
            byok: byok_from_lookup(&lookup),
            auth_scheme: match lookup {
                ModelLookup::Loaded(Some(e)) => e.info().auth_scheme,
                _ => AuthScheme::default(),
            },
        };
        let provider = match lookup {
            ModelLookup::Loaded(Some(e)) => e.effective_auth_provider().cloned(),
            _ => None,
        };
        (facts, provider)
    })
}
fn byok_from_lookup(lookup: &ModelLookup) -> ModelByok {
    match lookup {
        ModelLookup::ConfigUnavailable => ModelByok::Unknown,
        ModelLookup::Loaded(Some(e)) if e.has_own_credentials() => ModelByok::Byok,
        ModelLookup::Loaded(_) => ModelByok::NotByok,
    }
}
enum ModelLookup<'a> {
    /// `None` if `model_id` is absent from the catalog.
    Loaded(Option<&'a ModelEntry>),
    ConfigUnavailable,
}
/// Load + parse the effective config and hand the `model_id` lookup to `f`,
/// keeping "config unavailable" distinct from "model absent" so callers can
/// stay conservative on a transient config failure.
fn with_resolved_model<T>(model_id: &str, f: impl FnOnce(ModelLookup) -> T) -> T {
    let Some(raw) = crate::config::load_effective_config()
        .map_err(|e| tracing::warn!(error = %e, "config load failed for model auth lookup"))
        .ok()
    else {
        return f(ModelLookup::ConfigUnavailable);
    };
    let Some(cfg) = Config::new_from_toml_cfg(&raw)
        .map_err(|e| tracing::warn!(error = %e, "config parse failed for model auth lookup"))
        .ok()
    else {
        return f(ModelLookup::ConfigUnavailable);
    };
    let models = resolve_model_list(&cfg, None);
    f(ModelLookup::Loaded(find_model_by_id(&models, model_id)))
}
/// Resolve a standalone `SamplerConfig` for an auxiliary model slug (image
/// description, session summary, ...), resolved through the catalog so a
/// `[model.*]` override redirects it to its own endpoint, credentials, and
/// routing `model`. `None` → caller falls back to the active session's model.
pub(crate) fn resolve_aux_model_sampling_config(
    model_id: &str,
    models: &IndexMap<String, ModelEntry>,
    endpoints: &EndpointsConfig,
    session_key: Option<&str>,
    disable_api_key_auth: bool,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
) -> Option<SamplerConfig> {
    let catalog_entry = find_model_by_id(models, model_id).cloned();
    if let Some(entry) = &catalog_entry {
        let credentials = resolve_credentials_enforced(entry, session_key, disable_api_key_auth);
        let sampler = sampling_config_for_model(
            entry,
            credentials,
            alpha_test_key.clone(),
            client_version.clone(),
            None,
            None,
        );
        if sampler.api_key.is_some() {
            return Some(sampler);
        }
        if entry.effective_auth_provider().is_some() {
            tracing::warn!(
                model = %model_id,
                "aux model uses an auth provider with no cached token; the caller falls back to its session default"
            );
            return None;
        }
    }
    let echo_agent_bearer = session_key
        .map(|s| s.to_owned())
        .or_else(|| crate::agent::auth_method::read_echo_agent_api_key_env().ok())
        .or_else(|| endpoints.deployment_key.clone());
    if let Some(bearer) = echo_agent_bearer {
        let entry = ModelEntry {
            info: ModelInfo {
                user_selectable: true,
                id: None,
                model_family: None,
                model: catalog_entry
                    .map(|e| e.info.model)
                    .unwrap_or_else(|| model_id.to_owned()),
                base_url: endpoints.resolve_inference_base_url(),
                name: None,
                description: None,
                max_completion_tokens: None,
                temperature: None,
                top_p: None,
                api_backend: ApiBackend::Responses,
                auth_scheme: Default::default(),
                extra_headers: IndexMap::new(),
                query_params: IndexMap::new(),
                env_http_headers: IndexMap::new(),
                context_window: NonZeroU64::new(200_000).unwrap(),
                auto_compact_threshold_percent: None,
                system_prompt_label: None,
                use_concise: false,
                agent_type: default_agent_type(),
                inference_idle_timeout_secs: None,
                max_retries: None,
                subagent_rate_limit_max_attempts: None,
                hidden: true,
                supported_in_api: true,
                reasoning_effort: None,
                supports_reasoning_effort: false,
                reasoning_efforts: Vec::new(),
                supports_backend_search: false,
                compactions_remaining: None,
                compaction_at_tokens: None,
                show_model_fingerprint: false,
                stream_tool_calls: None,
                laziness_detector: LazinessDetectorPerModelConfig::default(),
            },
            api_key: Some(bearer),
            env_key: None,
            auth_provider: None,
            api_base_url: None,
        };
        let credentials = resolve_credentials_enforced(&entry, session_key, disable_api_key_auth);
        let sampler = sampling_config_for_model(
            &entry,
            credentials,
            alpha_test_key,
            client_version,
            None,
            None,
        );
        return Some(sampler);
    }
    tracing::warn!(
        aux_model = %model_id,
        "no credentials for auxiliary model; falling back to active model",
    );
    None
}
/// Stamp the session-local fields (client id, attribution, bearer resolver,
/// retries) from the active session onto a routed aux `SamplerConfig` so a
/// helper model keeps the session's auth/attribution. Shared by image-describe
/// and the auto-mode classifier so the two can't drift.
///
/// The resolver gate is host-based, stricter than `session_token_auth_gate`:
/// a session-token deployment on a custom `models_base_url` loses aux-sampler
/// refresh, rather than risk the session bearer on a third-party endpoint.
pub(crate) fn stamp_session_local_sampler_fields(
    cfg: &mut SamplerConfig,
    active_session_config: &SamplerConfig,
    client_identifier: Option<String>,
    max_retries: Option<u32>,
) {
    cfg.client_identifier = client_identifier;
    cfg.attribution_callback = active_session_config.attribution_callback.clone();
    if crate::util::is_echo_agent_api_bearer_url(&cfg.base_url) {
        cfg.bearer_resolver = active_session_config.bearer_resolver.clone();
    }
    cfg.max_retries = max_retries;
}
/// Finalize image-describe model + sampler config for user attachments.
/// Shared so the aux resolve happy path and the `None` fallback cannot
/// diverge between those entry points.
///
/// On aux resolve `Some`, stamp session-local fields onto the helper config.
/// On `None`, fall back to the active session model and full config (not
/// forcing `image_description_model` onto the agent endpoint, which 404s on
/// BYOK / non-proxy routes for internal slugs like `echo-agent-build`).
pub(crate) fn finalize_image_describe_sampler_config(
    resolved_aux: Option<SamplerConfig>,
    active_session_config: &SamplerConfig,
    client_identifier: Option<String>,
    max_retries: Option<u32>,
) -> (String, SamplerConfig) {
    match resolved_aux {
        Some(mut describe_cfg) => {
            stamp_session_local_sampler_fields(
                &mut describe_cfg,
                active_session_config,
                client_identifier,
                max_retries,
            );
            let model = describe_cfg.model.clone();
            (model, describe_cfg)
        }
        None => {
            let model = active_session_config.model.clone();
            (model, active_session_config.clone())
        }
    }
}
/// Re-derive `auth_type` from the model's own credentials so BYOK env-key
/// models stay on `ApiKey` even when a session token is present. Falls
/// back to `fallback` when the model isn't in the on-disk catalog.
pub(crate) fn resolve_chat_state_auth_type(
    model_id: &str,
    session_key: Option<&str>,
    fallback: echo_agent_chat_state::AuthType,
) -> echo_agent_chat_state::AuthType {
    try_resolve_model_credentials(model_id, session_key)
        .map(|r| r.auth_type)
        .unwrap_or(fallback)
}
/// Selects EchoAgent-only Responses extensions for trusted backend-search routes.
///
/// Third-party Responses providers reject `no_inline_citations`, so it must stay
/// on a trusted first-party route and apply only to models with backend search.
pub(crate) fn response_include_extensions(
    supports_backend_search: bool,
    api_backend: &ApiBackend,
    base_url: &str,
) -> Vec<String> {
    let is_trusted_route = crate::util::is_trusted_cli_chat_proxy_url(base_url)
        || crate::util::is_trusted_echo_agent_https_url(base_url);
    if supports_backend_search && api_backend == &ApiBackend::Responses && is_trusted_route {
        vec![NO_INLINE_CITATIONS_RESPONSE_INCLUDE.to_owned()]
    } else {
        Vec::new()
    }
}
pub(crate) fn sampling_config_for_model(
    model: &ModelEntry,
    credentials: ResolvedCredentials,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
    deployment_id: Option<String>,
    user_id: Option<String>,
) -> SamplerConfig {
    let info = model.info();
    let model_name = info.model.clone();
    let max_completion_tokens = info.max_completion_tokens;
    let temperature = info.temperature;
    let top_p = info.top_p;
    let mut extra_headers = info.extra_headers.clone();
    inject_url_derived_headers(
        &mut extra_headers,
        alpha_test_key.as_deref(),
        &credentials.base_url,
    );
    let api_backend = info.api_backend.clone();
    let extra_response_includes = response_include_extensions(
        info.supports_backend_search,
        &api_backend,
        &credentials.base_url,
    );
    SamplerConfig {
        api_key: credentials.api_key,
        model: model_name,
        base_url: credentials.base_url,
        max_completion_tokens,
        temperature,
        top_p,
        api_backend,
        auth_scheme: credentials.auth_scheme,
        extra_headers,
        extra_response_includes,
        query_params: info.query_params.clone(),
        env_http_headers: info.env_http_headers.clone(),
        context_window: info.context_window.get(),
        client_version,
        reasoning_effort: info.reasoning_effort,
        force_http1: false,
        max_retries: info.max_retries,
        stream_tool_calls: info.stream_tool_calls.unwrap_or(false),
        idle_timeout_secs: None,
        client_identifier: None,
        deployment_id,
        user_id,
        origin_client: None,
        attribution_callback: None,
        bearer_resolver: None,
        supports_backend_search: info.supports_backend_search,
        compactions_remaining: info.compactions_remaining,
        compaction_at_tokens: info.compaction_at_tokens,
        doom_loop_recovery: None,
        header_injector: None,
    }
}
/// Fold URL-derived headers into `extra_headers`.
///
/// The sampler crate is intentionally URL-agnostic: it does not inspect
/// `base_url` to decide which auth or staging headers to add. Replicate the
/// URL-derived header logic at the shell boundary so callers downstream see a
/// single homogenous header bag.
///
/// * cli-chat-proxy bases get `X-Echo-Agent-Token-Auth` and
///   `x-authenticateresponse` headers (mirrors the inline match in the legacy
///   `sampling::Client::new` on `is_cli_chat_proxy_url`).
/// * With the optional non-production feature, matching first-party hosts may
///   get an extra access header from the corresponding key argument.
///
/// Existing entries are never overwritten so callers can pre-set a value.
pub(crate) fn inject_url_derived_headers(
    headers: &mut IndexMap<String, String>,
    alpha_test_key: Option<&str>,
    base_url: &str,
) {
    if crate::util::is_cli_chat_proxy_url(base_url) {
        headers
            .entry("X-Echo-Agent-Token-Auth".to_string())
            .or_insert_with(|| "echo-agent-cli".to_string());
        headers
            .entry("x-authenticateresponse".to_string())
            .or_insert_with(|| "authenticate-response".to_string());
    }
    headers
        .entry(crate::http::CLIENT_MODE_HEADER.to_string())
        .or_insert_with(|| crate::http::process_client_mode().to_string());
    let _ = (alpha_test_key, base_url);
}
fn resolve_hidden_default_web_search_sampling_config(
    model_id: &str,
    session_key: Option<&str>,
    disable_api_key_auth: bool,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
    endpoints: &EndpointsConfig,
) -> SamplerConfig {
    let entry = ModelEntry {
        info: ModelInfo {
            id: None,
            model_family: None,
            model: model_id.to_owned(),
            base_url: endpoints.resolve_inference_base_url(),
            name: None,
            description: None,
            max_completion_tokens: None,
            temperature: None,
            top_p: None,
            api_backend: ApiBackend::Responses,
            auth_scheme: Default::default(),
            extra_headers: IndexMap::new(),
            query_params: IndexMap::new(),
            env_http_headers: IndexMap::new(),
            context_window: NonZeroU64::new(200_000).unwrap(),
            auto_compact_threshold_percent: None,
            system_prompt_label: None,
            use_concise: false,
            agent_type: default_agent_type(),
            inference_idle_timeout_secs: None,
            max_retries: None,
            subagent_rate_limit_max_attempts: None,
            hidden: true,
            user_selectable: true,
            supported_in_api: true,
            reasoning_effort: None,
            supports_reasoning_effort: false,
            reasoning_efforts: Vec::new(),
            supports_backend_search: false,
            compactions_remaining: None,
            compaction_at_tokens: None,
            show_model_fingerprint: false,
            stream_tool_calls: None,
            laziness_detector: LazinessDetectorPerModelConfig::default(),
        },
        api_key: None,
        env_key: None,
        auth_provider: None,
        api_base_url: None,
    };
    let credentials = resolve_credentials_enforced(&entry, session_key, disable_api_key_auth);
    sampling_config_for_model(
        &entry,
        credentials,
        alpha_test_key,
        client_version,
        None,
        None,
    )
}
pub(crate) fn resolve_web_search_sampling_config(
    model_id: &str,
    models: &IndexMap<String, ModelEntry>,
    session_key: Option<&str>,
    disable_api_key_auth: bool,
    alpha_test_key: Option<String>,
    client_version: Option<String>,
    endpoints: &EndpointsConfig,
) -> Option<SamplerConfig> {
    let resolved = if let Some(entry) = find_model_by_id(models, model_id).cloned() {
        let credentials = resolve_credentials_enforced(&entry, session_key, disable_api_key_auth);
        if credentials.api_key.is_none() && entry.effective_auth_provider().is_some() {
            tracing::warn!(
                web_search_model = %model_id,
                "web search model uses an auth provider with no cached token; disabling web search"
            );
            return None;
        }
        Some(sampling_config_for_model(
            &entry,
            credentials,
            alpha_test_key,
            client_version,
            None,
            None,
        ))
    } else if model_id == crate::models::default_web_search_model() {
        Some(resolve_hidden_default_web_search_sampling_config(
            model_id,
            session_key,
            disable_api_key_auth,
            alpha_test_key,
            client_version,
            endpoints,
        ))
    } else {
        None
    };
    if resolved.is_none() {
        tracing::warn!(
            web_search_model = %model_id,
            "configured web_search model not found; disabling web search"
        );
    }
    resolved.map(crate::tools::config::web_search_sampling_config)
}
pub(crate) fn to_acp_model_info(
    models: &IndexMap<String, ModelEntry>,
) -> IndexMap<acp::ModelId, acp::ModelInfo> {
    models
        .iter()
        .map(|(key, model)| {
            let info = model.info();
            let model_id = acp::ModelId::new(Arc::from(key.clone()));
            let total_context_tokens = info.context_window.get();
            let meta = {
                let mut map = serde_json::Map::new();
                map.insert(
                    "totalContextTokens".to_string(),
                    serde_json::Value::Number(total_context_tokens.into()),
                );
                map.insert(
                    "agentType".to_string(),
                    serde_json::Value::String(info.agent_type.clone()),
                );
                if info.supports_reasoning_effort {
                    map.insert(
                        "supportsReasoningEffort".to_string(),
                        serde_json::Value::Bool(true),
                    );
                    if let Some(effort) = info.reasoning_effort {
                        map.insert(
                            REASONING_EFFORT_META_KEY.to_string(),
                            reasoning_effort_meta_value(effort),
                        );
                    }
                }
                if !info.reasoning_efforts.is_empty() {
                    map.insert(
                        REASONING_EFFORTS_META_KEY.to_string(),
                        reasoning_efforts_meta_value(&info.reasoning_efforts),
                    );
                }
                if map.is_empty() { None } else { Some(map) }
            };
            (
                model_id.clone(),
                acp::ModelInfo::new(
                    model_id,
                    info.name.clone().unwrap_or_else(|| info.model.clone()),
                )
                .description(info.description.clone())
                .meta(meta),
            )
        })
        .collect()
}
/// Error code for model switch rejection due to agent type mismatch.
pub const MODEL_SWITCH_INCOMPATIBLE_AGENT: &str = "MODEL_SWITCH_INCOMPATIBLE_AGENT";
/// Error code for model switch failure during the zero-turn full harness
/// rebuild path. Emitted when `RebuildAgentForDefinition` fails (definition
/// could not be resolved at handler time, `AgentBuilder::build()` errored,
/// or a turn started racing the rebuild).
pub const MODEL_SWITCH_REBUILD_FAILED: &str = "MODEL_SWITCH_REBUILD_FAILED";
/// Structured error payload for model switch rejection due to agent type
/// incompatibility. Serialized into `acp::Error.data` by the shell and
/// deserialized by the TUI for user-friendly error rendering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ModelSwitchIncompatibleAgentError {
    /// Stable machine-readable error code (always `MODEL_SWITCH_INCOMPATIBLE_AGENT`).
    pub code: String,
    /// The agent type currently active in the session.
    pub active_agent_type: String,
    /// The agent type required by the target model.
    pub required_agent_type: String,
    /// The model ID that was requested.
    pub model_id: String,
    /// Remediation hint for the client.
    pub suggestion: String,
}
impl ModelSwitchIncompatibleAgentError {
    /// Build an `acp::Error` with this structured payload.
    pub(crate) fn into_acp_error(self) -> acp::Error {
        let message = format!(
            "Cannot switch to model '{}': it requires agent '{}' but the active agent is '{}'. \
             Start a new session to use this model.",
            self.model_id, self.required_agent_type, self.active_agent_type,
        );
        acp::Error::new(acp::ErrorCode::InvalidRequest.into(), message)
            .data(serde_json::to_value(&self).ok())
    }
    /// Try to parse from an `acp::Error.data` field.
    pub fn from_acp_error(err: &acp::Error) -> Option<Self> {
        let data = err.data.as_ref()?;
        let code = data.get("code")?.as_str()?;
        if code != MODEL_SWITCH_INCOMPATIBLE_AGENT {
            return None;
        }
        serde_json::from_value(data.clone()).ok()
    }
    /// Render a user-friendly error message for the TUI.
    pub fn user_message(&self) -> String {
        format!(
            "Cannot switch to '{}' — it requires agent '{}' but the active agent is '{}'. \
             Start /new to use this model.",
            self.model_id, self.required_agent_type, self.active_agent_type,
        )
    }
}
#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
