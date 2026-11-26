//! Shared OpenTelemetry tracing layer for exporting spans to the cli-chat-proxy.
//!
//! `echo-agent-pager` uses this module to set up OTLP
//! trace export so that session-level spans (with `session_id`, tool timings,
//! inference latency, etc.) are available in the product observability backend.
use crate::instrumentation;
use echo_agent_auth::AuthCredentialProvider;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::SdkTracerProvider;
use std::sync::{Arc, OnceLock};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::Layer as _;
use tracing_subscriber::registry::LookupSpan;
mod redact;
static TRACER_PROVIDER: OnceLock<SdkTracerProvider> = OnceLock::new();
const ENV_OTEL_FILTER: &str = "ECHO_AGENT_OTEL_FILTER";
const DEFAULT_OTEL_FILTER: &str = "info";
/// Configuration for [`build_otel_layer`]. Encapsulates all the runtime values
/// the layer needs that used to be reach-ins into shell-internal types
/// (`AuthManager`, `EndpointsConfig`, `EchoAgentComConfig`).
///
/// Built by the binaries (`echo-agent-pager`) from their own
/// configuration. The credentials provider is constructed by shell's
/// `echo_agent_runtime::auth::credential_provider::build_otel_credential_provider`.
pub struct OtelLayerConfig {
    /// Live credential source. Read on every batch export to obtain a fresh
    /// bearer token for the OTLP `Authorization` header.
    pub credentials: Arc<dyn AuthCredentialProvider>,
    /// Value for the `X-Echo-Agent-Token-Auth` header (typically `"echo-agent-cli"`).
    pub token_header_value: String,
    /// Optional extra access key for traces. Injection is honored only when
    /// the crate's optional non-production feature is enabled and only for
    /// matching first-party hosts. Field stays present so cross-crate
    /// constructors compile regardless of per-crate feature unification.
    pub alpha_test_key: Option<String>,
    pub exporter: OtelExporterConfig,
}
/// Static identity of the client emitting telemetry. Becomes resource
/// attributes (`client.name`, `client.version`, `service.version`,
/// `app.entrypoint`) on every span.
#[derive(Debug, Clone, Copy)]
pub struct OtelClientInfo {
    /// Binary name (`echo-agent-pager`) -> `client.name`.
    pub client_name: &'static str,
    /// Front-end client version -> `client.version`.
    pub client_version: &'static str,
    /// Engine build (version + commit) -> `service.version`.
    pub service_version: &'static str,
    /// How the session was launched (`cli`/`headless`/`agent`) -> `app.entrypoint`.
    pub app_entrypoint: &'static str,
}
/// OTLP trace-export transport settings, resolved from the `OTEL_*` env vars /
/// managed config.
#[derive(Debug, Default, Clone)]
pub struct OtelExporterConfig {
    /// Full OTLP traces endpoint URL (e.g. `https://chat-proxy.echo-agent.invalid/v1/traces`).
    pub traces_url: String,
    /// `OTEL_EXPORTER_OTLP_HEADERS` pairs.
    pub extra_headers: Vec<(String, String)>,
    /// `OTEL_TRACES_EXPORT_INTERVAL` batch flush interval. `None` = SDK default.
    pub export_interval: Option<std::time::Duration>,
    /// `OTEL_EXPORTER_OTLP_TIMEOUT` export timeout. `None` = 10s default.
    pub timeout: Option<std::time::Duration>,
    /// `false` when `OTEL_TRACES_EXPORTER=none`: spans created, never exported.
    pub enabled: bool,
}
/// Creates an OpenTelemetry layer that bridges tracing spans to OpenTelemetry.
/// This enables trace context propagation and OTLP export to the cli-chat-proxy.
///
/// - `client_name`: binary name (e.g. `"echo-agent-tui"`, `"echo-agent-pager"`) -- stored as
///   `client.name` resource attribute for dashboards to distinguish client types.
/// - `client_version`: `CARGO_PKG_VERSION` -- sent in the `x-echo-agent-client-version` header.
/// - `service_version`: `VERSION_WITH_COMMIT` -- stored as `service.version` resource attribute.
/// - `config`: runtime configuration; see [`OtelLayerConfig`].
pub fn build_otel_layer<S>(
    client: OtelClientInfo,
    config: OtelLayerConfig,
) -> impl tracing_subscriber::layer::Layer<S>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    let provider = TRACER_PROVIDER.get_or_init(|| build_tracer_provider(client, config));
    let tracer = provider.tracer("echo-agent-cli");
    global::set_tracer_provider(provider.clone());
    global::set_text_map_propagator(opentelemetry_sdk::propagation::TraceContextPropagator::new());
    let otel_filter =
        std::env::var(ENV_OTEL_FILTER).unwrap_or_else(|_| DEFAULT_OTEL_FILTER.to_string());
    let otel_filter = tracing_subscriber::filter::EnvFilter::try_new(&otel_filter)
        .unwrap_or_else(|e| {
            eprintln!(
                "[otel] Invalid ECHO_AGENT_OTEL_FILTER '{}': {}. Using default '{}'.",
                otel_filter, e, DEFAULT_OTEL_FILTER
            );
            tracing_subscriber::filter::EnvFilter::try_new(DEFAULT_OTEL_FILTER)
                .expect("default otel filter must parse")
        })
        .add_directive(
            "sampling_log=off"
                .parse()
                .expect("static directive must parse"),
        );
    OpenTelemetryLayer::new(tracer)
        .with_context_activation(false)
        .with_filter(otel_filter)
}
fn build_tracer_provider(client: OtelClientInfo, config: OtelLayerConfig) -> SdkTracerProvider {
    match instrumentation::current_mode() {
        instrumentation::InstrumentationMode::Server => build_server_provider(client, config),
        _ => SdkTracerProvider::builder().build(),
    }
}
/// Wraps an OTLP `SpanExporter`, rebuilding it with a fresh auth token on each
/// `export()` call if the in-memory auth token has changed. On export failure,
/// attempts a token refresh and retries once.
struct RefreshableSpanExporter {
    endpoint: Arc<str>,
    static_headers: Arc<std::collections::HashMap<String, String>>,
    credentials: Arc<dyn AuthCredentialProvider>,
    last_token: parking_lot::Mutex<String>,
    /// Pre-built HTTP client shared across all export calls. Created once at
    /// init (outside the batch processor thread) to avoid the "no reactor"
    /// panic that occurs when `hyper-util` tries DNS resolution on a non-Tokio
    /// thread.
    http_client: crate::otlp_http::BlockingOtlpClient,
    /// Resource set by the `BatchSpanProcessor` via `set_resource()`.
    /// Forwarded to each one-shot exporter so OTLP payloads include
    /// `service.name`, `service.version`, `user.id`, etc.
    resource: parking_lot::Mutex<opentelemetry_sdk::Resource>,
    /// Value for `X-Echo-Agent-Token-Auth`. Only sent when `credentials.needs_token_auth_header()`.
    token_header_value: Arc<str>,
    extra_headers: Arc<Vec<(String, String)>>,
}
impl std::fmt::Debug for RefreshableSpanExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshableSpanExporter")
            .field("endpoint", &self.endpoint)
            .field("static_headers", &self.static_headers)
            .field("credentials", &"configured")
            .field("last_token", &"***")
            .finish_non_exhaustive()
    }
}
/// Build the header map for an OTLP export request.
fn build_export_headers(
    static_headers: &std::collections::HashMap<String, String>,
    token: &str,
    token_auth_header: Option<&str>,
    extra_headers: &[(String, String)],
    snapshot: &echo_agent_auth::CredentialSnapshot,
) -> std::collections::HashMap<String, String> {
    let mut headers = static_headers.clone();
    for (name, value) in [
        ("x-userid", &snapshot.user_id),
        ("x-teamid", &snapshot.team_id),
    ] {
        match value.as_deref().filter(|v| !v.is_empty()) {
            Some(v) => {
                headers.insert(name.to_string(), v.to_string());
            }
            None => {
                headers.remove(name);
            }
        }
    }
    headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    if let Some(value) = token_auth_header {
        headers.insert("X-Echo-Agent-Token-Auth".to_string(), value.to_string());
    }
    for (k, v) in extra_headers {
        headers.insert(k.clone(), v.clone());
    }
    headers
}
fn build_otlp_exporter(
    endpoint: &str,
    static_headers: &std::collections::HashMap<String, String>,
    token: &str,
    token_auth_header: Option<&str>,
    extra_headers: &[(String, String)],
    http_client: crate::otlp_http::BlockingOtlpClient,
    snapshot: &echo_agent_auth::CredentialSnapshot,
) -> Result<opentelemetry_otlp::SpanExporter, opentelemetry_otlp::ExporterBuildError> {
    let headers = build_export_headers(
        static_headers,
        token,
        token_auth_header,
        extra_headers,
        snapshot,
    );
    opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(http_client)
        .with_endpoint(endpoint)
        .with_headers(headers)
        .build()
}
/// Send a batch through an exporter with `set_resource` applied.
async fn export_batch(
    exporter: &mut opentelemetry_otlp::SpanExporter,
    resource: &opentelemetry_sdk::Resource,
    batch: Vec<opentelemetry_sdk::trace::SpanData>,
) -> opentelemetry_sdk::error::OTelSdkResult {
    use opentelemetry_sdk::trace::SpanExporter as _;
    exporter.set_resource(resource);
    exporter.export(batch).await
}
impl RefreshableSpanExporter {
    #[cfg(test)]
    fn current_token(&self) -> String {
        self.credentials.snapshot().token.unwrap_or_else(|| {
            tracing::debug!("auth: otel credential snapshot has no token, using cached last_token");
            self.last_token.lock().clone()
        })
    }
}
/// Stamp `deployment.id`/`api_key.id`/`organization.id`/`team.id`/`user.id`
/// per-export (they're only known after auth is wired, post-init — stamping at
/// init would leave them blank for a session that authenticates mid-run).
fn resource_with_tenant_id(
    base: opentelemetry_sdk::Resource,
    snapshot: &echo_agent_auth::CredentialSnapshot,
) -> opentelemetry_sdk::Resource {
    let tenant_attrs: Vec<opentelemetry::KeyValue> = [
        ("deployment.id", &snapshot.deployment_id),
        ("api_key.id", &snapshot.api_key_id),
        ("organization.id", &snapshot.organization_id),
        ("team.id", &snapshot.team_id),
        ("user.id", &snapshot.user_id),
    ]
    .into_iter()
    .filter_map(|(key, val)| {
        val.as_deref()
            .filter(|v| !v.is_empty())
            .map(|v| opentelemetry::KeyValue::new(key, v.to_string()))
    })
    .collect();
    if tenant_attrs.is_empty() {
        return base;
    }
    let mut attrs: Vec<opentelemetry::KeyValue> = base
        .iter()
        .map(|(k, v)| opentelemetry::KeyValue::new(k.clone(), v.clone()))
        .collect();
    attrs.extend(tenant_attrs);
    opentelemetry_sdk::Resource::builder_empty()
        .with_attributes(attrs)
        .build()
}
/// Inputs for one export attempt, built on the calling thread: the
/// `BatchSpanProcessor` drives `export()` from a non-Tokio `std::thread`, so
/// constructing the exporter/HTTP client inside the future would hit the
/// "no reactor" panic.
struct ExportInputs {
    one_shot: Result<opentelemetry_otlp::SpanExporter, opentelemetry_otlp::ExporterBuildError>,
    resource: opentelemetry_sdk::Resource,
    credentials: Arc<dyn AuthCredentialProvider>,
    endpoint: Arc<str>,
    static_headers: Arc<std::collections::HashMap<String, String>>,
    token_header_value: Arc<str>,
    http_client: crate::otlp_http::BlockingOtlpClient,
    extra_headers: Arc<Vec<(String, String)>>,
}
impl opentelemetry_sdk::trace::SpanExporter for RefreshableSpanExporter {
    fn export(
        &self,
        batch: Vec<opentelemetry_sdk::trace::SpanData>,
    ) -> impl std::future::Future<Output = opentelemetry_sdk::error::OTelSdkResult> + Send {
        let prepared = (crate::client::is_session_metrics_enabled()
            && self.credentials.has_usable_credential())
        .then(|| {
            let snapshot = self.credentials.snapshot();
            let token = snapshot.token.clone().unwrap_or_else(|| {
                tracing::debug!(
                    "auth: otel credential snapshot has no token, using cached last_token"
                );
                self.last_token.lock().clone()
            });
            *self.last_token.lock() = token.clone();
            let token_auth = self
                .credentials
                .needs_token_auth_header()
                .then(|| Arc::clone(&self.token_header_value));
            ExportInputs {
                one_shot: build_otlp_exporter(
                    &self.endpoint,
                    &self.static_headers,
                    &token,
                    token_auth.as_deref(),
                    &self.extra_headers,
                    self.http_client.clone(),
                    &snapshot,
                ),
                resource: resource_with_tenant_id(self.resource.lock().clone(), &snapshot),
                credentials: Arc::clone(&self.credentials),
                endpoint: Arc::clone(&self.endpoint),
                static_headers: Arc::clone(&self.static_headers),
                token_header_value: Arc::clone(&self.token_header_value),
                http_client: self.http_client.clone(),
                extra_headers: Arc::clone(&self.extra_headers),
            }
        });
        async move {
            let Some(ExportInputs {
                one_shot,
                resource,
                credentials,
                endpoint,
                static_headers,
                token_header_value,
                http_client,
                extra_headers,
            }) = prepared
            else {
                return Ok(());
            };
            let mut exporter = match one_shot {
                Ok(e) => e,
                Err(e) => {
                    return Err(opentelemetry_sdk::error::OTelSdkError::InternalFailure(
                        format!("failed to build exporter: {e}"),
                    ));
                }
            };
            let mut batch = batch;
            redact::redact_batch(&mut batch);
            let batch_for_retry = tokio::runtime::Handle::try_current()
                .is_ok()
                .then(|| batch.clone());
            let result = export_batch(&mut exporter, &resource, batch).await;
            if result.is_ok() {
                return result;
            }
            let Some(batch_for_retry) = batch_for_retry else {
                return result;
            };
            tracing::debug!("otel export failed, attempting token refresh");
            if !credentials.refresh_after_unauthorized().await {
                return result;
            }
            let retry_snapshot = credentials.snapshot();
            let new_token = retry_snapshot.token.clone().unwrap_or_default();
            if new_token.is_empty() {
                tracing::warn!("token refresh reported success but snapshot returned no token");
                return result;
            }
            let retry_token_auth = credentials
                .needs_token_auth_header()
                .then(|| token_header_value.as_ref());
            match build_otlp_exporter(
                &endpoint,
                &static_headers,
                &new_token,
                retry_token_auth,
                &extra_headers,
                http_client,
                &retry_snapshot,
            ) {
                Ok(mut retry_exporter) => {
                    let retry_resource = resource_with_tenant_id(resource, &retry_snapshot);
                    export_batch(&mut retry_exporter, &retry_resource, batch_for_retry)
                        .await
                        .or(result)
                }
                Err(e) => {
                    tracing::debug!("failed to build retry exporter: {e}");
                    result
                }
            }
        }
    }
    fn set_resource(&mut self, resource: &opentelemetry_sdk::Resource) {
        *self.resource.lock() = resource.clone();
    }
    fn shutdown(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }
}
fn build_server_provider(client: OtelClientInfo, config: OtelLayerConfig) -> SdkTracerProvider {
    let OtelClientInfo {
        client_name,
        client_version,
        service_version,
        app_entrypoint,
    } = client;
    let snapshot = config.credentials.snapshot();
    let initial_token = snapshot.token.unwrap_or_default();
    if initial_token.is_empty() {
        tracing::debug!(
            "No authentication credentials found at init. OTLP exporter will retry after auth."
        );
    }
    let mut resource_attrs = vec![
        opentelemetry::KeyValue::new("service.version", service_version.to_string()),
        opentelemetry::KeyValue::new("client.name", client_name.to_string()),
        opentelemetry::KeyValue::new("client.version", client_version.to_string()),
        opentelemetry::KeyValue::new("app.entrypoint", app_entrypoint.to_string()),
    ];
    if let Some(terminal_type) = std::env::var("TERM_PROGRAM")
        .ok()
        .or_else(|| std::env::var("TERM").ok())
        .filter(|v| !v.is_empty())
    {
        resource_attrs.push(opentelemetry::KeyValue::new("terminal.type", terminal_type));
    }
    let mut provider = SdkTracerProvider::builder().with_resource(
        opentelemetry_sdk::Resource::builder_empty()
            .with_service_name("echo-agent-cli")
            .with_attributes(resource_attrs)
            .build(),
    );
    if config.exporter.enabled {
        let traces_url = config.exporter.traces_url;
        let mut static_headers = std::collections::HashMap::new();
        static_headers.insert(
            "x-echo-agent-client-version".to_string(),
            client_version.to_string(),
        );
        let timeout = config
            .exporter
            .timeout
            .unwrap_or(std::time::Duration::from_secs(10));
        let http_client = match crate::otlp_http::build_blocking_client(timeout, &[]) {
            Ok(client) => client,
            Err(err) => {
                tracing::warn!(error = %err, "otel: OTLP HTTP client build failed; span export disabled");
                return provider.build();
            }
        };
        let refreshable_exporter = RefreshableSpanExporter {
            endpoint: Arc::from(traces_url),
            static_headers: Arc::new(static_headers),
            credentials: config.credentials,
            last_token: parking_lot::Mutex::new(initial_token),
            http_client,
            resource: parking_lot::Mutex::new(opentelemetry_sdk::Resource::builder_empty().build()),
            token_header_value: Arc::from(config.token_header_value.as_str()),
            extra_headers: Arc::new(config.exporter.extra_headers),
        };
        let mut batch_builder =
            opentelemetry_sdk::trace::BatchConfigBuilder::default().with_max_export_batch_size(64);
        if let Some(interval) = config.exporter.export_interval {
            batch_builder = batch_builder.with_scheduled_delay(interval);
        }
        let batch_processor =
            opentelemetry_sdk::trace::BatchSpanProcessor::builder(refreshable_exporter)
                .with_batch_config(batch_builder.build())
                .build();
        provider = provider.with_span_processor(batch_processor);
    }
    provider.build()
}
/// Flush and shut down the global tracer provider (and the external OTEL
/// stream — both ride the same exit chokepoints).
///
/// Prefer [`OtelGuard`] for normal code paths. Use this directly only in
/// signal handlers or `process::exit` paths where destructors won't run.
/// Safe to call multiple times (second call logs a warning but does not panic;
/// the external shutdown is idempotent).
pub fn shutdown_otel() {
    crate::external::shutdown();
    if let Some(provider) = TRACER_PROVIDER.get()
        && let Err(e) = provider.shutdown()
    {
        tracing::debug!("[otel] Failed to shutdown tracer provider: {}", e);
    }
}
/// RAII guard that calls [`shutdown_otel`] on drop.
pub struct OtelGuard;
impl Drop for OtelGuard {
    fn drop(&mut self) {
        shutdown_otel();
    }
}
/// Create an [`OtelGuard`] that flushes traces on drop.
pub fn otel_guard() -> OtelGuard {
    OtelGuard
}
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use echo_agent_auth::{AuthCredentialProvider, CredentialSnapshot, HttpAuth};
    use std::sync::Mutex;
    /// Test double for `AuthCredentialProvider`. When constructed with
    /// `with_refresh`, `refresh_after_unauthorized` rotates the token and
    /// returns `true`; otherwise it returns `false`.
    struct TestProvider {
        token: Mutex<Option<String>>,
        refreshed_token: Option<String>,
        refresh_count: std::sync::atomic::AtomicU32,
    }
    impl TestProvider {
        fn new(initial: Option<&str>) -> Self {
            Self {
                token: Mutex::new(initial.map(|s| s.to_owned())),
                refreshed_token: None,
                refresh_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
        fn with_refresh(initial: &str, refreshed: &str) -> Self {
            Self {
                token: Mutex::new(Some(initial.to_owned())),
                refreshed_token: Some(refreshed.to_owned()),
                refresh_count: std::sync::atomic::AtomicU32::new(0),
            }
        }
        fn set(&self, value: Option<&str>) {
            *self.token.lock().unwrap() = value.map(|s| s.to_owned());
        }
        fn refresh_count(&self) -> u32 {
            self.refresh_count
                .load(std::sync::atomic::Ordering::Relaxed)
        }
    }
    impl HttpAuth for TestProvider {
        fn apply(
            &self,
            builder: reqwest::RequestBuilder,
            _base_url: &str,
        ) -> reqwest::RequestBuilder {
            builder
        }
    }
    #[async_trait]
    impl AuthCredentialProvider for TestProvider {
        fn snapshot(&self) -> CredentialSnapshot {
            CredentialSnapshot {
                token: self.token.lock().unwrap().clone(),
                ..Default::default()
            }
        }
        async fn refresh_after_unauthorized(&self) -> bool {
            if let Some(ref refreshed) = self.refreshed_token {
                *self.token.lock().unwrap() = Some(refreshed.clone());
                self.refresh_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                true
            } else {
                false
            }
        }
    }
    /// Must be called from a non-async test (`#[test]`, not `#[tokio::test]`).
    /// The blocking client spawns an internal tokio runtime that panics if
    /// dropped inside an async executor.
    fn make_exporter(
        provider: Arc<dyn AuthCredentialProvider>,
        last_token: &str,
    ) -> RefreshableSpanExporter {
        RefreshableSpanExporter {
            endpoint: Arc::from("http://localhost:4318/v1/traces"),
            static_headers: Arc::new(std::collections::HashMap::new()),
            credentials: provider,
            last_token: parking_lot::Mutex::new(last_token.to_string()),
            http_client: crate::otlp_http::build_blocking_client(
                std::time::Duration::from_secs(30),
                &[],
            )
            .expect("test OTLP HTTP client must build"),
            resource: parking_lot::Mutex::new(opentelemetry_sdk::Resource::builder().build()),
            token_header_value: Arc::from("echo-agent-cli"),
            extra_headers: Arc::new(Vec::new()),
        }
    }
    #[test]
    fn build_export_headers_tracks_snapshot_and_respects_overrides() {
        let static_headers = std::collections::HashMap::new();
        for snapshot in [
            CredentialSnapshot::default(),
            CredentialSnapshot {
                user_id: Some(String::new()),
                team_id: Some(String::new()),
                ..Default::default()
            },
        ] {
            let headers = build_export_headers(&static_headers, "tok", None, &[], &snapshot);
            assert!(!headers.contains_key("x-userid"));
            assert!(!headers.contains_key("x-teamid"));
        }
        let snapshot = CredentialSnapshot {
            user_id: Some("u1".into()),
            team_id: Some("t9".into()),
            ..Default::default()
        };
        let extra = vec![("Authorization".to_string(), "Bearer custom".to_string())];
        let headers = build_export_headers(&static_headers, "auto-token", None, &extra, &snapshot);
        assert_eq!(headers["x-userid"], "u1");
        assert_eq!(headers["x-teamid"], "t9");
        assert_eq!(headers["Authorization"], "Bearer custom");
    }
    #[test]
    fn refreshable_exporter_uses_updated_provider_token() {
        let provider = Arc::new(TestProvider::new(Some("token-a")));
        let exporter = make_exporter(provider.clone(), "cached-token");
        assert_eq!(exporter.current_token(), "token-a");
        provider.set(Some("token-b"));
        assert_eq!(exporter.current_token(), "token-b");
    }
    #[test]
    fn resource_injects_tenant_id_attrs() {
        use opentelemetry::Key;
        let base = opentelemetry_sdk::Resource::builder_empty()
            .with_attributes([opentelemetry::KeyValue::new("user.id", "")])
            .build();
        let plain = resource_with_tenant_id(base.clone(), &CredentialSnapshot::default());
        assert!(plain.get(&Key::from("deployment.id")).is_none());
        assert!(plain.get(&Key::from("api_key.id")).is_none());
        let snap = CredentialSnapshot {
            deployment_id: Some("dep-7b97".into()),
            ..Default::default()
        };
        let r = resource_with_tenant_id(base.clone(), &snap);
        assert_eq!(
            r.get(&Key::from("deployment.id")).map(|v| v.to_string()),
            Some("dep-7b97".to_string())
        );
        assert!(
            r.get(&Key::from("user.id")).is_some(),
            "base attrs preserved"
        );
        let snap = CredentialSnapshot {
            api_key_id: Some("ak-0c2b".into()),
            ..Default::default()
        };
        let r = resource_with_tenant_id(base.clone(), &snap);
        assert_eq!(
            r.get(&Key::from("api_key.id")).map(|v| v.to_string()),
            Some("ak-0c2b".to_string())
        );
        let snap = CredentialSnapshot {
            organization_id: Some("org-abc".into()),
            ..Default::default()
        };
        let r = resource_with_tenant_id(base.clone(), &snap);
        assert_eq!(
            r.get(&Key::from("organization.id")).map(|v| v.to_string()),
            Some("org-abc".to_string())
        );
        let snap = CredentialSnapshot {
            user_id: Some("user-42".into()),
            ..Default::default()
        };
        let r = resource_with_tenant_id(base.clone(), &snap);
        assert_eq!(
            r.get(&Key::from("user.id")).map(|v| v.to_string()),
            Some("user-42".to_string())
        );
        let snap = CredentialSnapshot {
            deployment_id: Some("dep-9".into()),
            user_id: Some(String::new()),
            organization_id: Some(String::new()),
            team_id: Some(String::new()),
            ..Default::default()
        };
        let r =
            resource_with_tenant_id(opentelemetry_sdk::Resource::builder_empty().build(), &snap);
        assert!(r.get(&Key::from("user.id")).is_none());
        assert!(r.get(&Key::from("organization.id")).is_none());
        assert!(r.get(&Key::from("team.id")).is_none());
        assert_eq!(
            r.get(&Key::from("deployment.id")).map(|v| v.to_string()),
            Some("dep-9".to_string())
        );
    }
    #[test]
    fn refreshable_exporter_falls_back_to_cached_token_when_provider_empty() {
        let provider = Arc::new(TestProvider::new(Some("expired-token")));
        let exporter = make_exporter(provider.clone(), "cached-token");
        assert_eq!(exporter.current_token(), "expired-token");
        provider.set(None);
        assert_eq!(exporter.current_token(), "cached-token");
    }
    #[tokio::test]
    async fn refresh_after_unauthorized_rotates_token() {
        let provider = TestProvider::with_refresh("stale-token", "fresh-token");
        assert_eq!(provider.snapshot().token.as_deref(), Some("stale-token"));
        assert!(provider.refresh_after_unauthorized().await);
        assert_eq!(provider.refresh_count(), 1);
        assert_eq!(provider.snapshot().token.as_deref(), Some("fresh-token"));
    }
    #[tokio::test]
    async fn no_refresh_when_provider_cannot_refresh() {
        let provider = TestProvider::new(Some("only-token"));
        assert!(!provider.refresh_after_unauthorized().await);
        assert_eq!(provider.refresh_count(), 0);
        assert_eq!(provider.snapshot().token.as_deref(), Some("only-token"));
    }
    /// A provider whose `refresh_after_unauthorized` requires a Tokio
    /// runtime (calls `tokio::task::spawn_blocking`), mimicking the real
    /// `OtelAuthCredentialProvider` → `try_lock_auth_file_async` path.
    struct TokioDependentProvider {
        refresh_count: std::sync::atomic::AtomicU32,
    }
    impl HttpAuth for TokioDependentProvider {
        fn apply(
            &self,
            builder: reqwest::RequestBuilder,
            _base_url: &str,
        ) -> reqwest::RequestBuilder {
            builder
        }
    }
    #[async_trait]
    impl AuthCredentialProvider for TokioDependentProvider {
        fn snapshot(&self) -> CredentialSnapshot {
            CredentialSnapshot {
                token: Some("test-token".into()),
                ..Default::default()
            }
        }
        async fn refresh_after_unauthorized(&self) -> bool {
            let _ = tokio::task::spawn_blocking(|| {}).await;
            self.refresh_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            true
        }
    }
    /// Regression test: export() must not call
    /// refresh_after_unauthorized() when driven by futures_executor on a
    /// plain std::thread (like the BatchSpanProcessor does).
    #[test]
    fn export_skips_refresh_without_tokio_runtime() {
        let provider = Arc::new(TokioDependentProvider {
            refresh_count: std::sync::atomic::AtomicU32::new(0),
        });
        let credentials: Arc<dyn AuthCredentialProvider> = provider.clone();
        let refresh_was_called = std::thread::spawn(move || {
            futures_executor::block_on(async {
                if tokio::runtime::Handle::try_current().is_err() {
                    return false;
                }
                credentials.refresh_after_unauthorized().await
            })
        })
        .join()
        .expect("thread must not panic");
        assert!(!refresh_was_called, "refresh must be skipped without Tokio");
        assert_eq!(
            provider
                .refresh_count
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "refresh_after_unauthorized must not be called"
        );
    }
    /// Verify refresh still works when a Tokio runtime IS present.
    #[tokio::test]
    async fn export_retries_refresh_with_tokio_runtime() {
        let provider = Arc::new(TokioDependentProvider {
            refresh_count: std::sync::atomic::AtomicU32::new(0),
        });
        let credentials: Arc<dyn AuthCredentialProvider> = provider.clone();
        let should_retry = async {
            if tokio::runtime::Handle::try_current().is_err() {
                return false;
            }
            credentials.refresh_after_unauthorized().await
        }
        .await;
        assert!(should_retry, "refresh should proceed with Tokio runtime");
        assert_eq!(
            provider
                .refresh_count
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }
}
