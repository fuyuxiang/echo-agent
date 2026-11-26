//! Provider construction for the external stream: `SdkLoggerProvider` +
//! `SdkMeterProvider`, never registered globally, never sharing anything with
//! the internal `RefreshableSpanExporter` pipeline.
//!
//! The exporters are plain `opentelemetry_otlp` http/protobuf or gRPC/protobuf
//! exporters built with **only** the customer headers from
//! `OTEL_EXPORTER_OTLP_HEADERS` — no code path here can attach
//! `Authorization`/`X-Echo-Agent-Token-Auth`/`x-userid`;
//! those constants live in `otel_layer` and are not referenced by this
//! module. No `AuthCredentialProvider` is ever read.

use std::sync::Arc;
use std::time::Duration;

use http::{HeaderMap, HeaderName, HeaderValue};
use opentelemetry_otlp::{
    Protocol, WithExportConfig, WithHttpConfig, WithTonicConfig,
    tonic_types::metadata::MetadataMap,
    tonic_types::transport::{Certificate, ClientTlsConfig, Identity},
};
use opentelemetry_sdk::logs::{
    BatchConfig, BatchConfigBuilder, BatchLogProcessor as ThreadBatchLogProcessor,
    LoggerProviderBuilder, SdkLoggerProvider,
    log_processor_with_async_runtime::BatchLogProcessor as RuntimeBatchLogProcessor,
};
use opentelemetry_sdk::metrics::{
    MeterProviderBuilder, PeriodicReader as ThreadPeriodicReader, SdkMeterProvider, Temporality,
    periodic_reader_with_async_runtime::PeriodicReader as RuntimePeriodicReader,
};
type BuildResult<T> = Result<T, opentelemetry_otlp::ExporterBuildError>;

type RuntimeCommand = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

#[derive(Clone)]
struct DedicatedRuntime {
    tx: tokio::sync::mpsc::UnboundedSender<RuntimeCommand>,
}

impl std::fmt::Debug for DedicatedRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DedicatedRuntime")
    }
}

impl DedicatedRuntime {
    fn new() -> Option<Self> {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RuntimeCommand>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<bool>();
        let pump = move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => {
                    let _ = ready_tx.send(true);
                    rt
                }
                Err(e) => {
                    tracing::error!(error = %e, "external OTEL gRPC runtime build failed; external telemetry disabled");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            rt.block_on(async move {
                while let Some(future) = rx.recv().await {
                    tokio::spawn(future);
                }
            });
        };
        let spawned = std::thread::Builder::new()
            .name("otel-external-rt".into())
            .spawn(pump);
        if let Err(e) = spawned {
            tracing::error!(error = %e, "external OTEL gRPC runtime thread spawn failed; external telemetry disabled");
            return None;
        }
        // Bounded: this runs on the startup path, and the host that refuses
        // threads is the one least likely to schedule this one promptly.
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(true) => Some(Self { tx }),
            _ => None,
        }
    }

    fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce() -> BuildResult<T> + Send + 'static,
    ) -> BuildResult<T> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Box::pin(async move {
                let _ = tx.send(f());
            }))
            .expect("external OTEL gRPC runtime thread must be alive");
        rx.recv()
            .expect("external OTEL gRPC runtime build response")
    }
}

impl opentelemetry_sdk::runtime::Runtime for DedicatedRuntime {
    fn spawn<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let _ = self.tx.send(Box::pin(future));
    }

    fn delay(&self, duration: Duration) -> impl std::future::Future<Output = ()> + Send + 'static {
        tokio::time::sleep(duration)
    }
}

impl opentelemetry_sdk::runtime::RuntimeChannel for DedicatedRuntime {
    type Receiver<T: std::fmt::Debug + Send> = tokio_stream::wrappers::ReceiverStream<T>;
    type Sender<T: std::fmt::Debug + Send> = tokio::sync::mpsc::Sender<T>;

    fn batch_message_channel<T: std::fmt::Debug + Send>(
        &self,
        capacity: usize,
    ) -> (Self::Sender<T>, Self::Receiver<T>) {
        let (sender, receiver) = tokio::sync::mpsc::channel(capacity);
        (
            sender,
            tokio_stream::wrappers::ReceiverStream::new(receiver),
        )
    }
}

use super::config::{ExporterSelection, ExternalOtelConfig, OtlpTransport, TemporalityPreference};
use super::redact::{ExportHealth, RedactingLogExporter, SharedGates, ValidatingMetricExporter};

/// Resource shared by both providers. `builder_empty()` (not `builder()`):
/// the default `EnvResourceDetector` would export `OTEL_RESOURCE_ATTRIBUTES`
/// env values, bypassing the schema (same rationale as the internal layer).
fn build_resource(cfg: &ExternalOtelConfig) -> opentelemetry_sdk::Resource {
    let mut attrs = vec![
        opentelemetry::KeyValue::new("service.version", cfg.client.service_version.clone()),
        opentelemetry::KeyValue::new("client.version", cfg.client.client_version.clone()),
        opentelemetry::KeyValue::new("app.entrypoint", cfg.client.app_entrypoint.clone()),
        opentelemetry::KeyValue::new(
            "echo_agent_code.schema.version",
            super::schema::SCHEMA_VERSION,
        ),
    ];
    // terminal.type: emulator brand (TERM_PROGRAM) or terminfo type (TERM).
    if let Some(terminal_type) = std::env::var("TERM_PROGRAM")
        .ok()
        .or_else(|| std::env::var("TERM").ok())
        .filter(|v| !v.is_empty())
    {
        attrs.push(opentelemetry::KeyValue::new("terminal.type", terminal_type));
    }
    opentelemetry_sdk::Resource::builder_empty()
        // RQ6 (final): `echo-agent-cli`, a wire commitment.
        .with_service_name("echo-agent-cli")
        .with_attributes(attrs)
        .build()
}

fn temporality(pref: TemporalityPreference) -> Temporality {
    match pref {
        TemporalityPreference::Delta => Temporality::Delta,
        TemporalityPreference::Cumulative => Temporality::Cumulative,
    }
}

/// Console (stderr) log exporter for local debugging
/// (`OTEL_LOGS_EXPORTER=console`). Writes to **stderr** so stdout protocol
/// channels (headless/stream-JSON) are never corrupted.
#[derive(Debug)]
struct StderrLogExporter;

impl opentelemetry_sdk::logs::LogExporter for StderrLogExporter {
    fn export(
        &self,
        batch: opentelemetry_sdk::logs::LogBatch<'_>,
    ) -> impl std::future::Future<Output = opentelemetry_sdk::error::OTelSdkResult> + Send {
        for (record, _scope) in batch.iter() {
            let attrs: Vec<String> = record
                .attributes_iter()
                .map(|(k, v)| format!("{}={v:?}", k.as_str()))
                .collect();
            eprintln!(
                "[external-otel] event={} {}",
                record.event_name().unwrap_or("?"),
                attrs.join(" ")
            );
        }
        std::future::ready(Ok(()))
    }
}

/// Console (stderr) metric exporter for local debugging.
#[derive(Debug)]
struct StderrMetricExporter {
    temporality: Temporality,
}

impl opentelemetry_sdk::metrics::exporter::PushMetricExporter for StderrMetricExporter {
    fn export(
        &self,
        metrics: &opentelemetry_sdk::metrics::data::ResourceMetrics,
    ) -> impl std::future::Future<Output = opentelemetry_sdk::error::OTelSdkResult> + Send {
        for scope in metrics.scope_metrics() {
            for metric in scope.metrics() {
                eprintln!(
                    "[external-otel] metric={} {:?}",
                    metric.name(),
                    metric.data()
                );
            }
        }
        std::future::ready(Ok(()))
    }

    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }

    fn shutdown_with_timeout(
        &self,
        _timeout: std::time::Duration,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        Ok(())
    }

    fn temporality(&self) -> Temporality {
        self.temporality
    }
}

/// Customer headers as the HTTP OTLP builder's header map. The **only** headers
/// the external HTTP exporters send (pinned by the header-isolation test below).
fn customer_headers(headers: &[(String, String)]) -> std::collections::HashMap<String, String> {
    headers.iter().cloned().collect()
}

/// Customer headers as gRPC metadata. Invalid metadata keys/values are skipped;
/// this mirrors the HTTP builder's "only customer-supplied headers" invariant
/// without letting one malformed entry disable telemetry entirely.
fn customer_metadata(input: &[(String, String)]) -> MetadataMap {
    let mut headers = HeaderMap::new();
    for (key, value) in input {
        let Ok(header_name) = HeaderName::try_from(key.as_str()) else {
            tracing::warn!(key = %key, "external otel: skipping invalid gRPC metadata key");
            continue;
        };
        let Ok(header_value) = HeaderValue::from_str(value) else {
            tracing::warn!(key = %key, "external otel: skipping invalid gRPC metadata value");
            continue;
        };
        headers.insert(header_name, header_value);
    }
    MetadataMap::from_headers(headers)
}

pub(crate) struct BuiltProviders {
    pub logger_provider: Option<SdkLoggerProvider>,
    pub meter_provider: Option<SdkMeterProvider>,
}

/// TLS configurations to try, in order, when building a gRPC exporter.
///
/// `opentelemetry-otlp` 0.32 must be handed an explicit `ClientTlsConfig` for
/// `https://` endpoints: its own fallback is `ClientTlsConfig::new()`, whose
/// root store is **empty** in tonic 0.14 (`Endpoint::from_shared` never
/// auto-enables roots), so every handshake would fail with `UnknownIssuer`.
///
/// For https endpoints this returns two candidates:
/// 1. system CA store + embedded webpki roots (+ the customer CA, if any);
/// 2. embedded webpki roots only (+ the customer CA, if any) — the fallback
///    for hosts whose native store is missing or unreadable, where tonic
///    fails candidate 1 at build time (`NativeCertsNotFound`). This keeps
///    parity with the HTTP transport's embedded-roots reqwest client.
///
/// For plain `http://` endpoints it returns a single `None` (no TLS).
/// `true` when the bytes contain at least one PEM certificate block.
///
/// The emptiness gate for fail-closed CA handling: a readable but cert-less
/// bundle must fail exporter construction, not silently fall back to the
/// default roots. Malformed blocks are caught later by the TLS stack's own
/// parser (also fail-closed, at exporter build).
pub(crate) fn pem_contains_certificate(pem: &[u8]) -> bool {
    const MARKER: &[u8] = b"-----BEGIN CERTIFICATE-----";
    pem.windows(MARKER.len()).any(|window| window == MARKER)
}

/// Assemble a PEM BEGIN line from fragments so source scanners do not treat
/// the marker itself as committed key material.
fn pem_begin_line(label_parts: &[&[u8]]) -> Vec<u8> {
    let mut line = Vec::with_capacity(32);
    line.extend_from_slice(b"-----BEGIN ");
    for part in label_parts {
        line.extend_from_slice(part);
    }
    line.extend_from_slice(b"-----");
    line
}

/// `true` when the bytes contain at least one PEM private-key block.
pub(crate) fn pem_contains_private_key(pem: &[u8]) -> bool {
    // Labels split so full `BEGIN … KEY` literals never appear contiguously.
    const LABELS: &[&[&[u8]]] = &[
        &[b"PRIVATE", b" KEY"],
        &[b"RSA ", b"PRIVATE", b" KEY"],
        &[b"EC ", b"PRIVATE", b" KEY"],
    ];
    LABELS.iter().any(|parts| {
        let marker = pem_begin_line(parts);
        pem.windows(marker.len()).any(|window| window == marker)
    })
}

/// Re-encode validated DER roots as one multi-block PEM string (tonic's
/// `Certificate::from_pem` parses every block in a single certificate).
/// `None` when `ders` is empty.
fn ders_to_pem_bundle(ders: &[Vec<u8>]) -> Option<String> {
    use base64::Engine as _;
    if ders.is_empty() {
        return None;
    }
    let mut pem = String::new();
    for der in ders {
        pem.push_str("-----BEGIN CERTIFICATE-----\n");
        pem.push_str(&base64::engine::general_purpose::STANDARD.encode(der));
        pem.push_str("\n-----END CERTIFICATE-----\n");
    }
    Some(pem)
}

fn grpc_tls_candidates(
    endpoint: &str,
    ca_certificate_path: Option<&str>,
    client_certificate_path: Option<&str>,
    client_key_path: Option<&str>,
) -> BuildResult<Vec<Option<ClientTlsConfig>>> {
    // Mirror opentelemetry-otlp's own `is_https` detection exactly (parsed
    // URI scheme == https, tonic/mod.rs) so its empty-root-store
    // `ClientTlsConfig::new()` fallback for https endpoints is unreachable:
    // every endpoint it treats as https gets an explicit config from us.
    // Schemeless or non-http(s) endpoints get no TLS config here and fail
    // fail-closed inside the exporter ("invalid URL, scheme is missing").
    let is_https = endpoint
        .parse::<http::Uri>()
        .ok()
        .and_then(|uri| uri.scheme().cloned())
        .is_some_and(|scheme| scheme == http::uri::Scheme::HTTPS);
    if !is_https {
        if client_certificate_path.is_some() || client_key_path.is_some() {
            tracing::warn!(
                endpoint,
                "external otel: mTLS client identity configured for a non-https \
                 gRPC endpoint; identity ignored (TLS is required for mTLS)"
            );
        }
        return Ok(vec![None]);
    }
    let mut base =
        ClientTlsConfig::new().trust_anchors(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    // Process-wide `ECHO_AGENT_EXTRA_CA_BUNDLE` roots (fail-open by that crate's
    // contract), matching the HTTP transport's client policy — the same
    // corporate CA must work on both transports.
    if let Some(extra_pem) = ders_to_pem_bundle(echo_agent_extra_ca::extra_root_ders()) {
        base = base.ca_certificate(Certificate::from_pem(extra_pem));
    }
    let base = match ca_certificate_path {
        // Fail closed on an unreadable or certificate-less customer CA (the
        // caller warns and disables the stream): silently exporting without
        // the configured trust anchor would be worse than not exporting.
        Some(path) => {
            let pem = std::fs::read(path).map_err(|e| {
                opentelemetry_otlp::ExporterBuildError::InternalFailure(format!(
                    "reading OTEL_EXPORTER_OTLP_CERTIFICATE {path:?}: {e}"
                ))
            })?;
            if !pem_contains_certificate(&pem) {
                return Err(opentelemetry_otlp::ExporterBuildError::InternalFailure(
                    format!("OTEL_EXPORTER_OTLP_CERTIFICATE {path:?} contains no certificates"),
                ));
            }
            base.ca_certificate(Certificate::from_pem(pem))
        }
        None => base,
    };
    let base = match (client_certificate_path, client_key_path) {
        (Some(cert_path), Some(key_path)) => {
            let cert_pem = std::fs::read(cert_path).map_err(|e| {
                opentelemetry_otlp::ExporterBuildError::InternalFailure(format!(
                    "reading OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE {cert_path:?}: {e}"
                ))
            })?;
            let key_pem = std::fs::read(key_path).map_err(|e| {
                opentelemetry_otlp::ExporterBuildError::InternalFailure(format!(
                    "reading OTEL_EXPORTER_OTLP_CLIENT_KEY {key_path:?}: {e}"
                ))
            })?;
            if !pem_contains_certificate(&cert_pem) {
                return Err(opentelemetry_otlp::ExporterBuildError::InternalFailure(
                    format!(
                        "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE {cert_path:?} contains no certificates"
                    ),
                ));
            }
            if !pem_contains_private_key(&key_pem) {
                return Err(opentelemetry_otlp::ExporterBuildError::InternalFailure(
                    format!("OTEL_EXPORTER_OTLP_CLIENT_KEY {key_path:?} contains no private key"),
                ));
            }
            base.identity(Identity::from_pem(cert_pem, key_pem))
        }
        (None, None) => base,
        _ => {
            return Err(opentelemetry_otlp::ExporterBuildError::InternalFailure(
                "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE and CLIENT_KEY must both be set".to_string(),
            ));
        }
    };
    Ok(vec![Some(base.clone().with_native_roots()), Some(base)])
}

/// Run `build` with each TLS candidate in order, returning the first success.
fn build_with_tls_fallback<T>(
    candidates: Vec<Option<ClientTlsConfig>>,
    mut build: impl FnMut(Option<ClientTlsConfig>) -> BuildResult<T>,
) -> BuildResult<T> {
    debug_assert!(!candidates.is_empty());
    let mut last_err = None;
    for candidate in candidates {
        match build(candidate) {
            Ok(exporter) => return Ok(exporter),
            Err(e) => {
                tracing::debug!(error = %e, "external otel: gRPC exporter TLS candidate failed");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.expect("at least one TLS candidate is always supplied"))
}

enum OtlpExportTransport<'a> {
    HttpProtobuf(&'a crate::otlp_http::BlockingOtlpClient),
    Grpc(&'a DedicatedRuntime),
}

trait OtlpExportFactory {
    type Exporter;

    fn export(&self, transport: OtlpExportTransport<'_>) -> BuildResult<Self::Exporter>;
}

struct OtlpLogExporterBuilder<'a> {
    cfg: &'a ExternalOtelConfig,
}

impl OtlpExportFactory for OtlpLogExporterBuilder<'_> {
    type Exporter = opentelemetry_otlp::LogExporter;

    fn export(&self, transport: OtlpExportTransport<'_>) -> BuildResult<Self::Exporter> {
        match transport {
            OtlpExportTransport::HttpProtobuf(http_client) => {
                opentelemetry_otlp::LogExporter::builder()
                    .with_http()
                    // Pin http/protobuf. opentelemetry-otlp's default protocol
                    // is compile-time, feature-gated: `http-json` (if unified
                    // into the build, as it is under Bazel) flips the default to
                    // JSON, while a pure-cargo build of this crate defaults to
                    // protobuf. Pin explicitly so the contract holds on every
                    // build when HTTP transport is selected.
                    .with_protocol(Protocol::HttpBinary)
                    .with_http_client(http_client.clone())
                    .with_endpoint(&self.cfg.logs_endpoint)
                    .with_headers(customer_headers(&self.cfg.logs_headers))
                    .build()
            }
            OtlpExportTransport::Grpc(runtime) => {
                let endpoint = self.cfg.logs_endpoint.clone();
                let timeout = self.cfg.timeout;
                let metadata = customer_metadata(&self.cfg.logs_headers);
                let tls_candidates = grpc_tls_candidates(
                    &endpoint,
                    self.cfg.logs_ca_certificate.as_deref(),
                    self.cfg.logs_client_certificate.as_deref(),
                    self.cfg.logs_client_key.as_deref(),
                )?;
                runtime.run(move || {
                    build_with_tls_fallback(tls_candidates, |tls| {
                        let mut builder = opentelemetry_otlp::LogExporter::builder()
                            .with_tonic()
                            .with_endpoint(endpoint.clone())
                            .with_timeout(timeout)
                            .with_metadata(metadata.clone());
                        if let Some(tls) = tls {
                            builder = builder.with_tls_config(tls);
                        }
                        builder.build()
                    })
                })
            }
        }
    }
}

struct OtlpMetricExporterBuilder<'a> {
    cfg: &'a ExternalOtelConfig,
    temporality: Temporality,
}

impl OtlpExportFactory for OtlpMetricExporterBuilder<'_> {
    type Exporter = opentelemetry_otlp::MetricExporter;

    fn export(&self, transport: OtlpExportTransport<'_>) -> BuildResult<Self::Exporter> {
        match transport {
            OtlpExportTransport::HttpProtobuf(http_client) => {
                opentelemetry_otlp::MetricExporter::builder()
                    .with_http()
                    // Pin http/protobuf (see the logs exporter above for the
                    // feature-unification rationale).
                    .with_protocol(Protocol::HttpBinary)
                    .with_http_client(http_client.clone())
                    .with_endpoint(&self.cfg.metrics_endpoint)
                    .with_headers(customer_headers(&self.cfg.metrics_headers))
                    .with_temporality(self.temporality)
                    .build()
            }
            OtlpExportTransport::Grpc(runtime) => {
                let endpoint = self.cfg.metrics_endpoint.clone();
                let timeout = self.cfg.timeout;
                let metadata = customer_metadata(&self.cfg.metrics_headers);
                let temporality = self.temporality;
                let tls_candidates = grpc_tls_candidates(
                    &endpoint,
                    self.cfg.metrics_ca_certificate.as_deref(),
                    self.cfg.metrics_client_certificate.as_deref(),
                    self.cfg.metrics_client_key.as_deref(),
                )?;
                runtime.run(move || {
                    build_with_tls_fallback(tls_candidates, |tls| {
                        let mut builder = opentelemetry_otlp::MetricExporter::builder()
                            .with_tonic()
                            .with_endpoint(endpoint.clone())
                            .with_timeout(timeout)
                            .with_metadata(metadata.clone())
                            .with_temporality(temporality);
                        if let Some(tls) = tls {
                            builder = builder.with_tls_config(tls);
                        }
                        builder.build()
                    })
                })
            }
        }
    }
}

fn build_log_otlp_provider(
    builder: LoggerProviderBuilder,
    cfg: &ExternalOtelConfig,
    batch_config: BatchConfig,
    http_client: Option<&crate::otlp_http::BlockingOtlpClient>,
    gates: SharedGates,
    health: Arc<ExportHealth>,
) -> BuildResult<LoggerProviderBuilder> {
    let exporter_builder = OtlpLogExporterBuilder { cfg };
    Ok(match cfg.logs_transport {
        OtlpTransport::HttpProtobuf => {
            let exporter = exporter_builder.export(OtlpExportTransport::HttpProtobuf(
                http_client.expect("client built for http/protobuf selection"),
            ))?;
            builder.with_log_processor(
                ThreadBatchLogProcessor::builder(RedactingLogExporter::new(
                    exporter, gates, health,
                ))
                .with_batch_config(batch_config)
                .build(),
            )
        }
        OtlpTransport::Grpc => {
            let Some(runtime) = DedicatedRuntime::new() else {
                return Err(opentelemetry_otlp::ExporterBuildError::ThreadSpawnFailed);
            };
            let exporter = exporter_builder.export(OtlpExportTransport::Grpc(&runtime))?;
            builder.with_log_processor(
                RuntimeBatchLogProcessor::builder(
                    RedactingLogExporter::new(exporter, gates, health),
                    runtime,
                )
                .with_batch_config(batch_config)
                .build(),
            )
        }
    })
}

fn build_metric_otlp_provider(
    builder: MeterProviderBuilder,
    cfg: &ExternalOtelConfig,
    http_client: Option<&crate::otlp_http::BlockingOtlpClient>,
    health: Arc<ExportHealth>,
) -> BuildResult<MeterProviderBuilder> {
    let exporter_builder = OtlpMetricExporterBuilder {
        cfg,
        temporality: temporality(cfg.temporality),
    };
    Ok(match cfg.metrics_transport {
        OtlpTransport::HttpProtobuf => {
            let exporter = exporter_builder.export(OtlpExportTransport::HttpProtobuf(
                http_client.expect("client built for http/protobuf selection"),
            ))?;
            builder.with_reader(
                ThreadPeriodicReader::builder(ValidatingMetricExporter::new(exporter, health))
                    .with_interval(cfg.metric_export_interval)
                    .build(),
            )
        }
        OtlpTransport::Grpc => {
            let Some(runtime) = DedicatedRuntime::new() else {
                return Err(opentelemetry_otlp::ExporterBuildError::ThreadSpawnFailed);
            };
            let exporter = exporter_builder.export(OtlpExportTransport::Grpc(&runtime))?;
            builder.with_reader(
                RuntimePeriodicReader::builder(
                    ValidatingMetricExporter::new(exporter, health),
                    runtime,
                )
                .with_interval(cfg.metric_export_interval)
                .build(),
            )
        }
    })
}

fn wrap_console_log_exporter(
    builder: LoggerProviderBuilder,
    batch_config: BatchConfig,
    gates: SharedGates,
    health: Arc<ExportHealth>,
) -> LoggerProviderBuilder {
    builder.with_log_processor(
        ThreadBatchLogProcessor::builder(RedactingLogExporter::new(
            StderrLogExporter,
            gates,
            health,
        ))
        .with_batch_config(batch_config)
        .build(),
    )
}

fn wrap_console_metric_exporter(
    builder: MeterProviderBuilder,
    cfg: &ExternalOtelConfig,
    health: Arc<ExportHealth>,
) -> MeterProviderBuilder {
    builder.with_reader(
        ThreadPeriodicReader::builder(ValidatingMetricExporter::new(
            StderrMetricExporter {
                temporality: temporality(cfg.temporality),
            },
            health,
        ))
        .with_interval(cfg.metric_export_interval)
        .build(),
    )
}

fn build_signal_http_client(
    cfg: &ExternalOtelConfig,
    ca_certificate: Option<&str>,
    client_certificate: Option<&str>,
    client_key: Option<&str>,
) -> Result<crate::otlp_http::BlockingOtlpClient, opentelemetry_otlp::ExporterBuildError> {
    let ca_files: Vec<&str> = ca_certificate.into_iter().collect();
    let identity = match (client_certificate, client_key) {
        (Some(certificate), Some(key)) => {
            Some(crate::otlp_http::ClientIdentityPaths { certificate, key })
        }
        (None, None) => None,
        _ => {
            return Err(opentelemetry_otlp::ExporterBuildError::InternalFailure(
                "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE and CLIENT_KEY must both be set".to_string(),
            ));
        }
    };
    crate::otlp_http::build_blocking_client_with_identity(cfg.timeout, &ca_files, identity)
        .map_err(opentelemetry_otlp::ExporterBuildError::InternalFailure)
}

/// Build the providers per the resolved config. Returns `None` providers for
/// signals whose exporter selection is `none`.
pub(crate) fn build(
    cfg: &ExternalOtelConfig,
    gates: SharedGates,
    health: Arc<ExportHealth>,
) -> Result<BuiltProviders, opentelemetry_otlp::ExporterBuildError> {
    let logs_http_client = (cfg.logs_transport == OtlpTransport::HttpProtobuf
        && cfg.logs_exporter == ExporterSelection::Otlp)
        .then(|| {
            build_signal_http_client(
                cfg,
                cfg.logs_ca_certificate.as_deref(),
                cfg.logs_client_certificate.as_deref(),
                cfg.logs_client_key.as_deref(),
            )
        })
        .transpose()?;
    let metrics_http_client = (cfg.metrics_transport == OtlpTransport::HttpProtobuf
        && cfg.metrics_exporter == ExporterSelection::Otlp)
        .then(|| {
            build_signal_http_client(
                cfg,
                cfg.metrics_ca_certificate.as_deref(),
                cfg.metrics_client_certificate.as_deref(),
                cfg.metrics_client_key.as_deref(),
            )
        })
        .transpose()?;

    // Console output is suppressed in the agent/headless entrypoints:
    // wrapping harnesses routinely capture stderr for diagnostics, and
    // interleaving periodic telemetry dumps there degrades those logs.
    let console_ok = !matches!(cfg.client.app_entrypoint.as_str(), "agent" | "headless");

    let logger_provider = match cfg.logs_exporter {
        ExporterSelection::None => None,
        ExporterSelection::Console if !console_ok => {
            tracing::debug!(
                "external otel: console logs exporter suppressed in agent/headless entrypoint"
            );
            None
        }
        selection => {
            let batch_config = BatchConfigBuilder::default()
                .with_scheduled_delay(cfg.logs_export_interval)
                .with_max_export_batch_size(64)
                .build();
            let builder = SdkLoggerProvider::builder().with_resource(build_resource(cfg));
            let provider = match selection {
                ExporterSelection::Otlp => build_log_otlp_provider(
                    builder,
                    cfg,
                    batch_config,
                    logs_http_client.as_ref(),
                    gates.clone(),
                    health.clone(),
                )?,
                _ => {
                    wrap_console_log_exporter(builder, batch_config, gates.clone(), health.clone())
                }
            };
            Some(provider.build())
        }
    };

    let meter_provider = match cfg.metrics_exporter {
        ExporterSelection::None => None,
        ExporterSelection::Console if !console_ok => {
            tracing::debug!(
                "external otel: console metrics exporter suppressed in agent/headless entrypoint"
            );
            None
        }
        selection => {
            let builder = SdkMeterProvider::builder().with_resource(build_resource(cfg));
            let provider = match selection {
                ExporterSelection::Otlp => build_metric_otlp_provider(
                    builder,
                    cfg,
                    metrics_http_client.as_ref(),
                    health.clone(),
                )?,
                _ => wrap_console_metric_exporter(builder, cfg, health.clone()),
            };
            Some(provider.build())
        }
    };

    Ok(BuiltProviders {
        logger_provider,
        meter_provider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external::config::ExternalOtelConfig;
    use std::collections::HashMap;

    fn cfg_with_headers(headers: Vec<(String, String)>) -> ExternalOtelConfig {
        let mut cfg = ExternalOtelConfig::resolve_with(
            |name| match name {
                "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
                "OTEL_LOGS_EXPORTER" => Some("otlp".into()),
                _ => None,
            },
            None,
        )
        .expect("test config must resolve");
        cfg.logs_headers = headers.clone();
        cfg.metrics_headers = headers;
        cfg
    }

    /// Header-isolation invariant (T2): the outgoing header map equals
    /// exactly the parsed `OTEL_EXPORTER_OTLP_HEADERS` — no `Authorization`,
    /// `X-Echo-Agent-Token-Auth`, `x-userid`, or `x-teamid` unless customer-supplied
    /// (complement of the internal pipeline's
    /// `extra_headers_override_bearer_but_keep_static_identity`).
    #[test]
    fn exporter_headers_are_exactly_customer_headers() {
        let cfg = cfg_with_headers(vec![("x-collector-token".into(), "abc".into())]);
        let headers = customer_headers(&cfg.logs_headers);
        let expected: HashMap<String, String> =
            [("x-collector-token".to_string(), "abc".to_string())].into();
        assert_eq!(headers, expected);
        for forbidden in [
            "Authorization",
            "X-Echo-Agent-Token-Auth",
            "x-userid",
            "x-teamid",
        ] {
            assert!(
                !headers.contains_key(forbidden),
                "{forbidden} must never be auto-attached to external exports"
            );
        }
    }

    #[test]
    fn customer_supplied_authorization_passes_through() {
        // The customer may auth their own collector however they want.
        let cfg = cfg_with_headers(vec![("Authorization".into(), "Bearer customer".into())]);
        assert_eq!(
            customer_headers(&cfg.logs_headers)
                .get("Authorization")
                .map(String::as_str),
            Some("Bearer customer")
        );
    }

    /// Regression test for GB-4580: with the opentelemetry-otlp 0.32 bump,
    /// the `tls-roots` feature stopped implying a TLS provider feature and
    /// every `https://` gRPC endpoint was rejected at exporter build time
    /// ("uses HTTPS but no TLS feature is enabled"), silently disabling the
    /// external stream. The exporters must build for https endpoints.
    #[test]
    fn grpc_exporters_build_for_https_endpoints() {
        let cfg = ExternalOtelConfig::resolve_with(
            |name| match name {
                "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
                "OTEL_LOGS_EXPORTER" | "OTEL_METRICS_EXPORTER" => Some("otlp".into()),
                "OTEL_EXPORTER_OTLP_PROTOCOL" => Some("grpc".into()),
                // Nothing listens here: gRPC channels connect lazily, so
                // exporter construction must still succeed.
                "OTEL_EXPORTER_OTLP_ENDPOINT" => Some("https://localhost:1".into()),
                _ => None,
            },
            None,
        )
        .expect("config must resolve");
        let gates: SharedGates = Arc::new(parking_lot::RwLock::new(Default::default()));
        let health = Arc::new(ExportHealth::default());
        let built = super::build(&cfg, gates, health)
            .expect("https gRPC exporters must build (GB-4580 regression)");
        assert!(built.logger_provider.is_some());
        assert!(built.meter_provider.is_some());
    }

    #[test]
    fn grpc_tls_candidates_plain_http_has_no_tls() {
        let candidates = grpc_tls_candidates("http://localhost:4317", None, None, None)
            .expect("http must resolve");
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].is_none());
    }

    #[test]
    fn grpc_tls_candidates_https_tries_native_then_embedded_roots() {
        let candidates =
            grpc_tls_candidates("https://collector.corp.example:4317", None, None, None)
                .expect("https");
        assert_eq!(
            candidates.len(),
            2,
            "native-roots candidate + embedded fallback"
        );
        assert!(candidates.iter().all(Option::is_some));
    }

    /// Endpoints without a scheme must not get a TLS config: they agree with
    /// opentelemetry-otlp's own https detection (parsed scheme), and the
    /// exporter rejects them at connect time ("scheme is missing") rather
    /// than handshaking with an empty root store.
    #[test]
    fn grpc_tls_candidates_schemeless_endpoint_gets_no_tls() {
        let candidates = grpc_tls_candidates("collector.corp.example:4317", None, None, None)
            .expect("schemeless");
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].is_none());
    }

    /// Scheme detection is on the parsed URI, so case differences cannot
    /// diverge from the exporter's own https check.
    #[test]
    fn grpc_tls_candidates_uppercase_https_scheme_detected() {
        let candidates =
            grpc_tls_candidates("HTTPS://collector.corp.example:4317", None, None, None)
                .expect("https");
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(Option::is_some));
    }

    /// A CA override on an *inactive* signal must not take down the active
    /// signal's per-signal HTTP client (and with it the whole stream).
    #[test]
    fn inactive_signal_ca_does_not_disable_http_stream() {
        let cfg = ExternalOtelConfig::resolve_with(
            |name| match name {
                "ECHO_AGENT_EXTERNAL_OTEL" => Some("1".into()),
                // Only metrics export; logs are off but carry a broken CA.
                "OTEL_METRICS_EXPORTER" => Some("otlp".into()),
                "OTEL_EXPORTER_OTLP_LOGS_CERTIFICATE" => {
                    Some("/nonexistent/inactive-signal-ca.pem".into())
                }
                _ => None,
            },
            None,
        )
        .expect("config must resolve");
        assert_eq!(cfg.logs_exporter, ExporterSelection::None);
        let gates: SharedGates = Arc::new(parking_lot::RwLock::new(Default::default()));
        let health = Arc::new(ExportHealth::default());
        let built = super::build(&cfg, gates, health)
            .expect("inactive signal's CA must not fail the active stream");
        assert!(built.logger_provider.is_none());
        assert!(built.meter_provider.is_some());
    }

    /// A readable but certificate-less CA bundle must fail closed, not build
    /// exporters that verify without the configured trust anchor.
    #[test]
    fn grpc_tls_candidates_fail_closed_on_empty_ca_file() {
        let file = tempfile::NamedTempFile::new().expect("temp CA file");
        std::fs::write(file.path(), "# readable, but no PEM certificate blocks\n")
            .expect("write empty bundle");
        let err = grpc_tls_candidates(
            "https://collector.corp.example:4317",
            Some(file.path().to_str().expect("utf-8 path")),
            None,
            None,
        )
        .expect_err("certificate-less bundle must fail exporter construction");
        assert!(err.to_string().contains("no certificates"), "{err}");
    }

    #[test]
    fn pem_certificate_detection() {
        assert!(pem_contains_certificate(
            b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"
        ));
        assert!(!pem_contains_certificate(b""));
        // A non-certificate PEM block (CSR) must not count — note it does not
        // contain the exact `-----BEGIN CERTIFICATE-----` marker.
        assert!(!pem_contains_certificate(
            b"-----BEGIN CERTIFICATE REQUEST-----\nAAAA\n-----END CERTIFICATE REQUEST-----\n"
        ));
    }

    #[test]
    fn pem_private_key_detection() {
        let mut pkcs8 = pem_begin_line(&[b"PRIVATE", b" KEY"]);
        pkcs8.extend_from_slice(b"\nAAAA\n-----END ");
        pkcs8.extend_from_slice(b"PRIVATE");
        pkcs8.extend_from_slice(b" KEY-----\n");
        assert!(pem_contains_private_key(&pkcs8));

        let mut rsa = pem_begin_line(&[b"RSA ", b"PRIVATE", b" KEY"]);
        rsa.extend_from_slice(b"\nAAAA\n-----END RSA ");
        rsa.extend_from_slice(b"PRIVATE");
        rsa.extend_from_slice(b" KEY-----\n");
        assert!(pem_contains_private_key(&rsa));

        assert!(!pem_contains_private_key(b""));
        assert!(!pem_contains_private_key(
            b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n"
        ));
    }

    #[test]
    fn grpc_tls_candidates_fail_closed_on_empty_client_key() {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};

        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca");
        let client_key = KeyPair::generate().expect("client key");
        let client_params =
            CertificateParams::new(vec!["client".to_string()]).expect("client params");
        let client_cert = client_params
            .signed_by(&client_key, &ca_cert, &ca_key)
            .expect("sign client");

        let cert_file = tempfile::NamedTempFile::new().expect("cert file");
        let key_file = tempfile::NamedTempFile::new().expect("key file");
        std::fs::write(cert_file.path(), client_cert.pem()).expect("write cert");
        std::fs::write(key_file.path(), "# no private key block\n").expect("write empty key");

        let err = grpc_tls_candidates(
            "https://collector.corp.example:4317",
            None,
            Some(cert_file.path().to_str().expect("utf-8")),
            Some(key_file.path().to_str().expect("utf-8")),
        )
        .expect_err("empty client key must fail exporter construction");
        assert!(err.to_string().contains("CLIENT_KEY"), "{err}");
    }

    /// The DER→PEM re-encode used for `ECHO_AGENT_EXTRA_CA_BUNDLE` must produce a
    /// bundle other PEM parsers can read back, one block per DER.
    #[test]
    fn ders_to_pem_bundle_roundtrips() {
        assert!(ders_to_pem_bundle(&[]).is_none());
        let key = rcgen::KeyPair::generate().expect("key");
        let cert = rcgen::CertificateParams::new(vec!["localhost".into()])
            .expect("params")
            .self_signed(&key)
            .expect("cert");
        let der = cert.der().to_vec();
        let pem = ders_to_pem_bundle(&[der.clone(), der]).expect("bundle");
        let parsed = reqwest::Certificate::from_pem_bundle(pem.as_bytes()).expect("parse back");
        assert_eq!(parsed.len(), 2);
        assert!(pem_contains_certificate(pem.as_bytes()));
    }

    #[test]
    fn grpc_tls_candidates_fail_closed_on_missing_ca_file() {
        let err = grpc_tls_candidates(
            "https://collector.corp.example:4317",
            Some("/nonexistent/corp-ca.pem"),
            None,
            None,
        )
        .expect_err("missing CA bundle must fail exporter construction");
        assert!(
            err.to_string().contains("OTEL_EXPORTER_OTLP_CERTIFICATE"),
            "{err}"
        );
    }

    #[test]
    fn grpc_tls_candidates_fail_closed_on_missing_client_identity() {
        let err = grpc_tls_candidates(
            "https://collector.corp.example:4317",
            None,
            Some("/nonexistent/client.crt"),
            Some("/nonexistent/client.key"),
        )
        .expect_err("missing client cert must fail exporter construction");
        assert!(
            err.to_string()
                .contains("OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE"),
            "{err}"
        );
    }

    #[test]
    fn grpc_tls_candidates_accepts_client_identity_files() {
        use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};

        let ca_key = KeyPair::generate().expect("ca key");
        let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let ca_cert = ca_params.self_signed(&ca_key).expect("ca");

        let client_key = KeyPair::generate().expect("client key");
        let client_params =
            CertificateParams::new(vec!["client".to_string()]).expect("client params");
        let client_cert = client_params
            .signed_by(&client_key, &ca_cert, &ca_key)
            .expect("sign client");

        let cert_file = tempfile::NamedTempFile::new().expect("cert file");
        let key_file = tempfile::NamedTempFile::new().expect("key file");
        std::fs::write(cert_file.path(), client_cert.pem()).expect("write cert");
        std::fs::write(key_file.path(), client_key.serialize_pem()).expect("write key");

        let candidates = grpc_tls_candidates(
            "https://collector.corp.example:4317",
            None,
            Some(cert_file.path().to_str().expect("utf-8")),
            Some(key_file.path().to_str().expect("utf-8")),
        )
        .expect("valid client identity must build TLS candidates");
        assert_eq!(candidates.len(), 2);
        assert!(candidates.iter().all(Option::is_some));
    }

    #[test]
    fn exporter_metadata_is_customer_headers_only() {
        let cfg = cfg_with_headers(vec![
            ("x-collector-token".into(), "abc".into()),
            ("bad header".into(), "skip".into()),
        ]);
        let metadata = customer_metadata(&cfg.logs_headers);
        assert_eq!(
            metadata
                .get("x-collector-token")
                .and_then(|v| v.to_str().ok()),
            Some("abc")
        );
        for forbidden in ["x-echo-agent-token-auth", "x-userid", "x-teamid"] {
            assert!(metadata.get(forbidden).is_none());
        }
    }
}
