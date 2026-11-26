//! Configuration resolution for the external OTEL stream.
//!
//! Pure resolution — no I/O besides reading env vars. The shell resolves the
//! startup value once (layering the `[telemetry]` `otel_*` config keys under
//! the env vars) and passes the resolved struct to [`crate::external::init`].
//!
//! Activation requires a **double opt-in** (user-confirmed, RQ7):
//! `ECHO_AGENT_EXTERNAL_OTEL=1` *and* at least one of `OTEL_METRICS_EXPORTER` /
//! `OTEL_LOGS_EXPORTER` set to a real exporter. The master switch alone
//! enables nothing; the exporter vars alone enable nothing.

use std::time::Duration;

/// OTLP transport/protocol for external exporters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OtlpTransport {
    /// OTLP over HTTP with protobuf bodies.
    #[default]
    HttpProtobuf,
    /// OTLP over gRPC/protobuf.
    Grpc,
}

impl OtlpTransport {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "http/protobuf" | "http-protobuf" | "http" => Some(Self::HttpProtobuf),
            "grpc" => Some(Self::Grpc),
            "" => Some(Self::HttpProtobuf),
            _ => None,
        }
    }

    pub fn as_protocol_str(self) -> &'static str {
        match self {
            Self::HttpProtobuf => "http/protobuf",
            Self::Grpc => "grpc",
        }
    }
}

/// Master switch env var. Deliberately *not* `ECHO_AGENT_ENABLE_TELEMETRY`: that
/// would be a word-order typo away from the long-standing
/// `ECHO_AGENT_TELEMETRY_ENABLED` (product events/Mixpanel mode), and the two control
/// opposite-pointing data flows (to EchoAgent vs. to the customer's collector).
pub const ENV_MASTER_SWITCH: &str = "ECHO_AGENT_EXTERNAL_OTEL";

/// Exporter selection for one signal (`OTEL_METRICS_EXPORTER` /
/// `OTEL_LOGS_EXPORTER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExporterSelection {
    /// No exporter — the signal is not produced.
    #[default]
    None,
    /// OTLP to the configured endpoint using [`OtlpTransport`].
    Otlp,
    /// Redacted records printed to **stderr** (debugging). Stdout protocol
    /// channels (headless/stream-JSON) are never touched.
    Console,
}

impl ExporterSelection {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "otlp" => Some(Self::Otlp),
            "console" => Some(Self::Console),
            "none" | "" => Some(Self::None),
            _ => None,
        }
    }

    /// `true` for any selection that produces output.
    pub fn is_active(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Content gates (additive opt-ins; default off). May only **tighten**
/// post-init — a remote policy can force them off, never on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentGates {
    /// `OTEL_LOG_USER_PROMPTS=1`: prompt text on `echo_agent_code.user_prompt`
    /// (60 KB cap, secret-scrubbed).
    pub log_user_prompts: bool,
    /// `OTEL_LOG_TOOL_DETAILS=1`: gated tool params / full paths / verbatim
    /// MCP, skill, and plugin names.
    pub log_tool_details: bool,
}

/// Delta vs. cumulative metric temporality
/// (`OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE`). Default **Delta**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TemporalityPreference {
    #[default]
    Delta,
    Cumulative,
}

/// Identity of the binary emitting external telemetry; becomes resource
/// attributes. Filled by the caller (pager/shell) at init.
#[derive(Debug, Clone, Default)]
pub struct ExternalClientInfo {
    /// Engine build (version + commit) → `service.version`.
    pub service_version: String,
    /// Front-end client version → `client.version`.
    pub client_version: String,
    /// How the session was launched (`cli`/`headless`/`agent`) →
    /// `app.entrypoint`.
    pub app_entrypoint: String,
}

/// Config-file layer for the external stream, built by the shell from the
/// `otel_*` keys of the `[telemetry]` table and layered *under* env vars
/// during resolution. (Field names here are the internal carrier; the
/// user-facing keys are `otel_enabled`, `otel_metrics_exporter`, … — see
/// [`crate::config::TelemetryConfig`].)
///
/// There is deliberately **no `headers` key** (user decision, RQ4): collector
/// auth is supplied via the `OTEL_EXPORTER_OTLP_HEADERS` env var only, so
/// collector tokens are never stored on disk.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ExternalOtelFileConfig {
    /// `= ECHO_AGENT_EXTERNAL_OTEL` (env wins).
    pub enabled: Option<bool>,
    /// `otlp` | `console` | `none`.
    pub metrics_exporter: Option<String>,
    /// `otlp` | `console` | `none`.
    pub logs_exporter: Option<String>,
    /// OTLP base endpoint (`/v1/logs`, `/v1/metrics` appended per spec for HTTP).
    pub endpoint: Option<String>,
    /// `http/protobuf` | `grpc`.
    pub protocol: Option<String>,
    pub certificate: Option<String>,
    pub client_certificate: Option<String>,
    pub client_key: Option<String>,
    /// Content gate (admins can pin this to `false` via requirements).
    pub log_user_prompts: Option<bool>,
    /// Content gate (admins can pin this to `false` via requirements).
    pub log_tool_details: Option<bool>,
}

/// Fully resolved configuration for the external stream. Returned by
/// [`ExternalOtelConfig::resolve`] only when the double opt-in is satisfied;
/// `None` means the module is never constructed (zero allocation, zero
/// threads, zero sockets).
#[derive(Debug, Clone)]
pub struct ExternalOtelConfig {
    pub metrics_exporter: ExporterSelection,
    pub logs_exporter: ExporterSelection,
    pub logs_transport: OtlpTransport,
    pub metrics_transport: OtlpTransport,
    /// Resolved logs endpoint (full `…/v1/logs` for HTTP; collector origin for gRPC).
    pub logs_endpoint: String,
    /// Resolved metrics endpoint (full `…/v1/metrics` for HTTP; collector origin for gRPC).
    pub metrics_endpoint: String,
    /// Customer collector headers for log exports, parsed from
    /// `OTEL_EXPORTER_OTLP_HEADERS` plus `OTEL_EXPORTER_OTLP_LOGS_HEADERS`.
    /// The **only** headers the external log exporter ever sends.
    pub logs_headers: Vec<(String, String)>,
    /// Customer collector headers for metric exports, parsed from
    /// `OTEL_EXPORTER_OTLP_HEADERS` plus `OTEL_EXPORTER_OTLP_METRICS_HEADERS`.
    /// The **only** headers the external metric exporter ever sends.
    pub metrics_headers: Vec<(String, String)>,
    /// PEM file with additional trusted CA certificate(s) for verifying the
    /// logs collector (`OTEL_EXPORTER_OTLP_CERTIFICATE`, overridden by
    /// `OTEL_EXPORTER_OTLP_LOGS_CERTIFICATE`). Additive to the default roots.
    pub logs_ca_certificate: Option<String>,
    /// Same for the metrics collector (`OTEL_EXPORTER_OTLP_CERTIFICATE`,
    /// overridden by `OTEL_EXPORTER_OTLP_METRICS_CERTIFICATE`).
    pub metrics_ca_certificate: Option<String>,
    pub logs_client_certificate: Option<String>,
    pub logs_client_key: Option<String>,
    pub metrics_client_certificate: Option<String>,
    pub metrics_client_key: Option<String>,
    /// `OTEL_EXPORTER_OTLP_TIMEOUT` (ms). Default 10 s.
    pub timeout: Duration,
    /// `OTEL_METRIC_EXPORT_INTERVAL` (ms). Default 60 s.
    pub metric_export_interval: Duration,
    /// `OTEL_BLRP_SCHEDULE_DELAY` (spec name, wins) /
    /// `OTEL_LOGS_EXPORT_INTERVAL` (compatibility alias). Default 5 s.
    pub logs_export_interval: Duration,
    pub gates: ContentGates,
    pub temporality: TemporalityPreference,
    /// `OTEL_METRICS_INCLUDE_SESSION_ID` (default on): `session.id` on
    /// metrics (cardinality opt-out).
    pub include_session_id_on_metrics: bool,
    /// `OTEL_METRICS_INCLUDE_VERSION` (default off): `app.version` on
    /// metrics.
    pub include_version_on_metrics: bool,
    /// Resource identity, filled by the caller at init.
    pub client: ExternalClientInfo,
    /// Set by the shell when the **internal** firehose resolved its
    /// endpoint/headers from `OTEL_EXPORTER_OTLP_*` (the deprecated
    /// fallback). [`crate::external::init`] refuses to activate when true —
    /// the no-double-send invariant is enforced in code, not release
    /// discipline.
    pub internal_pipeline_consumed_otel_vars: bool,
    /// Which layer supplied the master switch (`"env"` | `"config"`), for the
    /// internal adoption meta-event. `remote` is not a possible startup
    /// source (init reads env + local config only).
    pub enabled_source: &'static str,
}

fn env_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

fn parse_ms(raw: Option<String>, default: Duration) -> Duration {
    raw.and_then(|s| s.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(default)
}

/// Parse `k=v,k2=v2` header lists (OTLP env spec); blank keys skipped.
pub fn parse_header_list(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let k = k.trim();
            (!k.is_empty()).then(|| (k.to_string(), v.trim().to_string()))
        })
        .collect()
}

/// OTLP HTTP default base endpoint per spec.
const DEFAULT_OTLP_HTTP_BASE: &str = "http://localhost:4318";
/// OTLP gRPC default endpoint per spec.
const DEFAULT_OTLP_GRPC_ENDPOINT: &str = "http://localhost:4317";

fn resolve_client_identity(
    signal: &str,
    certificate: Option<String>,
    key: Option<String>,
    cert_source: &str,
    key_source: &str,
) -> (Option<String>, Option<String>) {
    match (certificate, key) {
        (Some(cert), Some(key)) => (Some(cert), Some(key)),
        (None, None) => (None, None),
        (Some(_), None) => {
            tracing::warn!(
                signal,
                source = cert_source,
                "external otel: client certificate set without matching client key; \
                 mTLS identity ignored for this signal"
            );
            (None, None)
        }
        (None, Some(_)) => {
            tracing::warn!(
                signal,
                source = key_source,
                "external otel: client key set without matching client certificate; \
                 mTLS identity ignored for this signal"
            );
            (None, None)
        }
    }
}

/// Resolve a cert/key pair where the upper layer (env) is atomic over the lower
/// layer (file or base): a half-set upper layer never crosses with the lower
/// layer's complementary path.
fn resolve_identity_layer(
    signal: &str,
    upper_certificate: Option<String>,
    upper_key: Option<String>,
    upper_cert_source: &str,
    upper_key_source: &str,
    lower_certificate: Option<String>,
    lower_key: Option<String>,
    lower_cert_source: &str,
    lower_key_source: &str,
) -> (Option<String>, Option<String>) {
    let upper_any = upper_certificate.is_some() || upper_key.is_some();
    if upper_any {
        let upper_partial = upper_certificate.is_some() ^ upper_key.is_some();
        let lower_complete = lower_certificate.is_some() && lower_key.is_some();
        // A single leftover upper-layer variable must not silently discard a
        // complete managed/base pair: the only symptom is an empty collector.
        if upper_partial && lower_complete {
            let set_source = if upper_certificate.is_some() {
                upper_cert_source
            } else {
                upper_key_source
            };
            tracing::warn!(
                signal,
                set = set_source,
                discarded_cert = lower_cert_source,
                discarded_key = lower_key_source,
                "external otel: half-set upper identity discards complete lower-layer \
                 mTLS pair; mTLS identity ignored for this signal"
            );
            return (None, None);
        }
        return resolve_client_identity(
            signal,
            upper_certificate,
            upper_key,
            upper_cert_source,
            upper_key_source,
        );
    }
    resolve_client_identity(
        signal,
        lower_certificate,
        lower_key,
        lower_cert_source,
        lower_key_source,
    )
}

fn resolve_signal_endpoint(
    signal_specific: Option<String>,
    base: Option<&str>,
    path: &str,
    transport: OtlpTransport,
) -> String {
    if let Some(full) = signal_specific.filter(|s| !s.trim().is_empty()) {
        return full.trim().trim_end_matches('/').to_string();
    }
    let default_base = match transport {
        OtlpTransport::HttpProtobuf => DEFAULT_OTLP_HTTP_BASE,
        OtlpTransport::Grpc => DEFAULT_OTLP_GRPC_ENDPOINT,
    };
    let base = base
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(default_base)
        .trim()
        .trim_end_matches('/');
    match transport {
        OtlpTransport::HttpProtobuf => format!("{base}/{path}"),
        OtlpTransport::Grpc => base.to_owned(),
    }
}

impl ExternalOtelConfig {
    /// Resolve from process env layered over the optional `[telemetry]`
    /// `otel_*` config-file layer. Returns `None` unless the double opt-in is
    /// satisfied (master switch + at least one real exporter) and the
    /// transport is supported.
    pub fn resolve(file: Option<&ExternalOtelFileConfig>) -> Option<Self> {
        Self::resolve_with(|name| std::env::var(name).ok(), file)
    }

    /// Testable resolution core: `getenv` abstracts `std::env::var` so tests
    /// don't race on process-global env state.
    pub fn resolve_with(
        getenv: impl Fn(&str) -> Option<String>,
        file: Option<&ExternalOtelFileConfig>,
    ) -> Option<Self> {
        // Master switch: env > config file > default off.
        let (enabled, enabled_source) =
            match getenv(ENV_MASTER_SWITCH).as_deref().and_then(env_bool) {
                Some(v) => (v, "env"),
                None => match file.and_then(|f| f.enabled) {
                    Some(v) => (v, "config"),
                    None => (false, "env"),
                },
            };
        if !enabled {
            return None;
        }

        let select = |env_name: &str, file_value: Option<&str>| -> ExporterSelection {
            let raw = getenv(env_name).or_else(|| file_value.map(str::to_owned));
            match raw.as_deref().map(ExporterSelection::parse) {
                Some(Some(sel)) => sel,
                Some(None) => {
                    tracing::warn!(
                        var = env_name,
                        "external otel: unrecognized exporter selection; treating as `none`"
                    );
                    ExporterSelection::None
                }
                None => ExporterSelection::None,
            }
        };
        let metrics_exporter = select(
            "OTEL_METRICS_EXPORTER",
            file.and_then(|f| f.metrics_exporter.as_deref()),
        );
        let logs_exporter = select(
            "OTEL_LOGS_EXPORTER",
            file.and_then(|f| f.logs_exporter.as_deref()),
        );
        // Double opt-in (RQ7): the master switch alone enables nothing.
        if !metrics_exporter.is_active() && !logs_exporter.is_active() {
            return None;
        }

        // Unrecognized protocol on an *active* signal disables the stream; on an
        // *inactive* signal it is ignored so a stray fleet env var for a disabled
        // signal cannot take down the other.
        let resolve_protocol = |raw: Option<String>,
                                label: &str,
                                signal_active: bool|
         -> Option<Option<OtlpTransport>> {
            match raw {
                // Blank is treated as unset so signal vars inherit the base protocol
                // instead of `parse("")` forcing HttpProtobuf.
                None => Some(None),
                Some(s) if s.trim().is_empty() => Some(None),
                Some(s) => match OtlpTransport::parse(&s) {
                    Some(t) => Some(Some(t)),
                    None if signal_active => {
                        tracing::warn!(
                            protocol = %s,
                            signal = label,
                            "external otel: unrecognized OTLP protocol; stream disabled"
                        );
                        None
                    }
                    None => {
                        tracing::warn!(
                            protocol = %s,
                            signal = label,
                            "external otel: unrecognized OTLP protocol on inactive signal; ignoring"
                        );
                        Some(None)
                    }
                },
            }
        };
        // Base is soft: an invalid generic protocol must not block valid
        // per-signal overrides (OTLP signal-over-generic precedence). Only
        // active signals that actually inherit a missing/invalid base fail.
        #[derive(Clone, Copy)]
        enum BaseProtocol {
            Explicit(OtlpTransport),
            Default,
            Invalid,
        }
        let base_protocol = {
            let raw = getenv("OTEL_EXPORTER_OTLP_PROTOCOL")
                .or_else(|| file.and_then(|f| f.protocol.clone()));
            match raw {
                None => BaseProtocol::Default,
                Some(s) if s.trim().is_empty() => BaseProtocol::Default,
                Some(s) => match OtlpTransport::parse(&s) {
                    Some(t) => BaseProtocol::Explicit(t),
                    None => {
                        tracing::warn!(
                            protocol = %s,
                            "external otel: unrecognized base OTLP protocol; \
                             active signals without a signal-specific protocol will fail"
                        );
                        BaseProtocol::Invalid
                    }
                },
            }
        };
        let inherit_base = |signal_active: bool| -> Option<OtlpTransport> {
            match base_protocol {
                BaseProtocol::Explicit(t) => Some(t),
                BaseProtocol::Default => Some(OtlpTransport::HttpProtobuf),
                BaseProtocol::Invalid if signal_active => {
                    tracing::warn!(
                        "external otel: active signal inherits invalid base OTLP protocol; \
                         stream disabled"
                    );
                    None
                }
                BaseProtocol::Invalid => Some(OtlpTransport::HttpProtobuf),
            }
        };
        let logs_otlp = logs_exporter == ExporterSelection::Otlp;
        let metrics_otlp = metrics_exporter == ExporterSelection::Otlp;
        let logs_transport = match resolve_protocol(
            getenv("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL"),
            "logs",
            logs_otlp,
        )? {
            Some(t) => t,
            None => inherit_base(logs_otlp)?,
        };
        let metrics_transport = match resolve_protocol(
            getenv("OTEL_EXPORTER_OTLP_METRICS_PROTOCOL"),
            "metrics",
            metrics_otlp,
        )? {
            Some(t) => t,
            None => inherit_base(metrics_otlp)?,
        };

        let base_endpoint = getenv("OTEL_EXPORTER_OTLP_ENDPOINT")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| file.and_then(|f| f.endpoint.clone()))
            .filter(|s| !s.trim().is_empty());
        let logs_endpoint = resolve_signal_endpoint(
            getenv("OTEL_EXPORTER_OTLP_LOGS_ENDPOINT"),
            base_endpoint.as_deref(),
            "v1/logs",
            logs_transport,
        );
        let metrics_endpoint = resolve_signal_endpoint(
            getenv("OTEL_EXPORTER_OTLP_METRICS_ENDPOINT"),
            base_endpoint.as_deref(),
            "v1/metrics",
            metrics_transport,
        );

        // Headers: env only (RQ4) — never from the config file. Resolve them
        // per signal so signal-specific overrides never bleed across streams.
        let base_headers = parse_header_list(
            getenv("OTEL_EXPORTER_OTLP_HEADERS")
                .as_deref()
                .unwrap_or(""),
        );
        let resolve_signal_headers = |signal_var: &str| {
            let mut headers = base_headers.clone();
            if let Some(extra) = getenv(signal_var) {
                for (k, v) in parse_header_list(&extra) {
                    if let Some(existing) = headers.iter_mut().find(|(ek, _)| *ek == k) {
                        existing.1 = v;
                    } else {
                        headers.push((k, v));
                    }
                }
            }
            headers
        };
        let logs_headers = resolve_signal_headers("OTEL_EXPORTER_OTLP_LOGS_HEADERS");
        let metrics_headers = resolve_signal_headers("OTEL_EXPORTER_OTLP_METRICS_HEADERS");

        let base_certificate = getenv("OTEL_EXPORTER_OTLP_CERTIFICATE")
            .filter(|s| !s.trim().is_empty())
            .or_else(|| file.and_then(|f| f.certificate.clone()))
            .filter(|s| !s.trim().is_empty());
        let resolve_signal_certificate = |signal_var: &str| {
            getenv(signal_var)
                .filter(|s| !s.trim().is_empty())
                .or_else(|| base_certificate.clone())
                .map(|s| s.trim().to_string())
        };
        let logs_ca_certificate = resolve_signal_certificate("OTEL_EXPORTER_OTLP_LOGS_CERTIFICATE");
        let metrics_ca_certificate =
            resolve_signal_certificate("OTEL_EXPORTER_OTLP_METRICS_CERTIFICATE");

        let env_client_certificate = getenv("OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE")
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string());
        let env_client_key = getenv("OTEL_EXPORTER_OTLP_CLIENT_KEY")
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string());
        let file_client_certificate = file
            .and_then(|f| f.client_certificate.clone())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string());
        let file_client_key = file
            .and_then(|f| f.client_key.clone())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string());
        let (base_client_certificate, base_client_key) = resolve_identity_layer(
            "base",
            env_client_certificate,
            env_client_key,
            "env OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
            "env OTEL_EXPORTER_OTLP_CLIENT_KEY",
            file_client_certificate,
            file_client_key,
            "config [telemetry].otel_client_certificate",
            "config [telemetry].otel_client_key",
        );
        let signal_identity = |signal: &str, cert_var: &str, key_var: &str| {
            let signal_cert = getenv(cert_var)
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string());
            let signal_key = getenv(key_var)
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string());
            resolve_identity_layer(
                signal,
                signal_cert,
                signal_key,
                &format!("env {cert_var}"),
                &format!("env {key_var}"),
                base_client_certificate.clone(),
                base_client_key.clone(),
                "resolved base client certificate",
                "resolved base client key",
            )
        };
        let (logs_client_certificate, logs_client_key) = signal_identity(
            "logs",
            "OTEL_EXPORTER_OTLP_LOGS_CLIENT_CERTIFICATE",
            "OTEL_EXPORTER_OTLP_LOGS_CLIENT_KEY",
        );
        let (metrics_client_certificate, metrics_client_key) = signal_identity(
            "metrics",
            "OTEL_EXPORTER_OTLP_METRICS_CLIENT_CERTIFICATE",
            "OTEL_EXPORTER_OTLP_METRICS_CLIENT_KEY",
        );

        let gates = ContentGates {
            log_user_prompts: getenv("OTEL_LOG_USER_PROMPTS")
                .as_deref()
                .and_then(env_bool)
                .or_else(|| file.and_then(|f| f.log_user_prompts))
                .unwrap_or(false),
            log_tool_details: getenv("OTEL_LOG_TOOL_DETAILS")
                .as_deref()
                .and_then(env_bool)
                .or_else(|| file.and_then(|f| f.log_tool_details))
                .unwrap_or(false),
        };

        let temporality = match getenv("OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE")
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("cumulative") => TemporalityPreference::Cumulative,
            // `delta`, `lowmemory`, unset, or unrecognized → Delta default.
            _ => TemporalityPreference::Delta,
        };

        Some(Self {
            metrics_exporter,
            logs_exporter,
            logs_transport,
            metrics_transport,
            logs_endpoint,
            metrics_endpoint,
            logs_headers,
            metrics_headers,
            logs_ca_certificate,
            metrics_ca_certificate,
            logs_client_certificate,
            logs_client_key,
            metrics_client_certificate,
            metrics_client_key,
            timeout: parse_ms(
                getenv("OTEL_EXPORTER_OTLP_TIMEOUT"),
                Duration::from_millis(10_000),
            ),
            metric_export_interval: parse_ms(
                getenv("OTEL_METRIC_EXPORT_INTERVAL"),
                Duration::from_millis(60_000),
            ),
            logs_export_interval: parse_ms(
                getenv("OTEL_BLRP_SCHEDULE_DELAY").or_else(|| getenv("OTEL_LOGS_EXPORT_INTERVAL")),
                Duration::from_millis(5_000),
            ),
            gates,
            temporality,
            include_session_id_on_metrics: getenv("OTEL_METRICS_INCLUDE_SESSION_ID")
                .as_deref()
                .and_then(env_bool)
                .unwrap_or(true),
            include_version_on_metrics: getenv("OTEL_METRICS_INCLUDE_VERSION")
                .as_deref()
                .and_then(env_bool)
                .unwrap_or(false),
            client: ExternalClientInfo::default(),
            internal_pipeline_consumed_otel_vars: false,
            enabled_source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn default_is_off() {
        assert!(ExternalOtelConfig::resolve_with(env(&[]), None).is_none());
    }

    #[test]
    fn master_switch_alone_enables_nothing() {
        // RQ7: ECHO_AGENT_EXTERNAL_OTEL=1 without an explicit exporter is inert.
        assert!(
            ExternalOtelConfig::resolve_with(env(&[("ECHO_AGENT_EXTERNAL_OTEL", "1")]), None)
                .is_none()
        );
    }

    #[test]
    fn exporters_alone_enable_nothing() {
        assert!(
            ExternalOtelConfig::resolve_with(env(&[("OTEL_METRICS_EXPORTER", "otlp")]), None)
                .is_none()
        );
    }

    #[test]
    fn double_opt_in_activates() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
            ]),
            None,
        )
        .expect("must activate");
        assert_eq!(cfg.metrics_exporter, ExporterSelection::Otlp);
        assert_eq!(cfg.logs_exporter, ExporterSelection::None);
        assert_eq!(cfg.metrics_endpoint, "http://localhost:4318/v1/metrics");
        assert_eq!(cfg.logs_endpoint, "http://localhost:4318/v1/logs");
        assert!(!cfg.gates.log_user_prompts);
        assert!(!cfg.gates.log_tool_details);
        assert!(cfg.include_session_id_on_metrics);
        assert!(!cfg.include_version_on_metrics);
        assert_eq!(cfg.temporality, TemporalityPreference::Delta);
        assert_eq!(cfg.logs_transport, OtlpTransport::HttpProtobuf);
        assert_eq!(cfg.metrics_transport, OtlpTransport::HttpProtobuf);
    }

    #[test]
    fn grpc_protocol_accepted() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
            ]),
            None,
        )
        .expect("grpc must activate");
        assert_eq!(cfg.logs_transport, OtlpTransport::Grpc);
        assert_eq!(cfg.metrics_transport, OtlpTransport::Grpc);
        assert_eq!(cfg.logs_endpoint, "http://localhost:4317");
    }

    #[test]
    fn http_protobuf_protocol_accepted() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
            ]),
            None,
        );
        let cfg = cfg.unwrap();
        assert_eq!(cfg.logs_transport, OtlpTransport::HttpProtobuf);
        assert_eq!(cfg.metrics_transport, OtlpTransport::HttpProtobuf);
    }

    #[test]
    fn unknown_protocol_disables() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
            ]),
            None,
        );
        assert!(cfg.is_none(), "unknown protocols must disable the stream");
    }

    #[test]
    fn endpoint_resolution_follows_otlp_http_spec() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                (
                    "OTEL_EXPORTER_OTLP_ENDPOINT",
                    "https://collector.corp.example:4318/",
                ),
                (
                    "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
                    "https://logs.corp.example/custom",
                ),
            ]),
            None,
        )
        .unwrap();
        // Signal-specific endpoint used verbatim; base + spec path otherwise.
        assert_eq!(cfg.logs_endpoint, "https://logs.corp.example/custom");
        assert_eq!(
            cfg.metrics_endpoint,
            "https://collector.corp.example:4318/v1/metrics"
        );
    }

    #[test]
    fn grpc_endpoint_resolution_uses_collector_endpoint_without_http_paths() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
                (
                    "OTEL_EXPORTER_OTLP_ENDPOINT",
                    "https://collector.corp.example:4317/",
                ),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.logs_endpoint, "https://collector.corp.example:4317");
        assert_eq!(cfg.metrics_endpoint, "https://collector.corp.example:4317");
    }

    #[test]
    fn file_protocol_layered_under_env() {
        let file = ExternalOtelFileConfig {
            enabled: Some(true),
            metrics_exporter: None,
            logs_exporter: Some("otlp".into()),
            endpoint: None,
            log_user_prompts: None,
            log_tool_details: None,
            protocol: Some("grpc".into()),
            ..Default::default()
        };
        let cfg = ExternalOtelConfig::resolve_with(env(&[]), Some(&file)).unwrap();
        assert_eq!(cfg.logs_transport, OtlpTransport::Grpc);
        assert_eq!(cfg.metrics_transport, OtlpTransport::Grpc);

        let cfg = ExternalOtelConfig::resolve_with(
            env(&[("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf")]),
            Some(&file),
        )
        .unwrap();
        assert_eq!(cfg.logs_transport, OtlpTransport::HttpProtobuf);
        assert_eq!(cfg.metrics_transport, OtlpTransport::HttpProtobuf);
    }

    #[test]
    fn per_signal_protocol_overrides_base() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
                ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "grpc"),
                (
                    "OTEL_EXPORTER_OTLP_ENDPOINT",
                    "https://collector.corp.example:4318",
                ),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.logs_transport, OtlpTransport::Grpc);
        assert_eq!(cfg.metrics_transport, OtlpTransport::HttpProtobuf);
        assert_eq!(cfg.logs_endpoint, "https://collector.corp.example:4318");
        assert_eq!(
            cfg.metrics_endpoint,
            "https://collector.corp.example:4318/v1/metrics"
        );
    }

    #[test]
    fn unrecognized_signal_protocol_disables_active_signal() {
        assert!(
            ExternalOtelConfig::resolve_with(
                env(&[
                    ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                    ("OTEL_LOGS_EXPORTER", "otlp"),
                    ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json"),
                ]),
                None,
            )
            .is_none()
        );
    }

    #[test]
    fn unrecognized_protocol_on_inactive_signal_is_ignored() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                // logs exporter is off: a bad logs protocol must not kill metrics
                ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "http/json"),
            ]),
            None,
        )
        .expect("inactive signal protocol must not disable the stream");
        assert_eq!(cfg.metrics_exporter, ExporterSelection::Otlp);
        assert_eq!(cfg.metrics_transport, OtlpTransport::HttpProtobuf);
    }

    #[test]
    fn empty_signal_protocol_inherits_base() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "grpc"),
                ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "  "),
                ("OTEL_EXPORTER_OTLP_METRICS_PROTOCOL", ""),
            ]),
            None,
        )
        .expect("blank signal protocol must inherit base");
        assert_eq!(cfg.logs_transport, OtlpTransport::Grpc);
        assert_eq!(cfg.metrics_transport, OtlpTransport::Grpc);
    }

    #[test]
    fn invalid_base_protocol_allows_valid_signal_overrides() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
                ("OTEL_EXPORTER_OTLP_LOGS_PROTOCOL", "grpc"),
                ("OTEL_EXPORTER_OTLP_METRICS_PROTOCOL", "http/protobuf"),
            ]),
            None,
        )
        .expect("valid signal protocols must recover from invalid base");
        assert_eq!(cfg.logs_transport, OtlpTransport::Grpc);
        assert_eq!(cfg.metrics_transport, OtlpTransport::HttpProtobuf);
    }

    #[test]
    fn invalid_base_protocol_disables_when_active_signal_inherits() {
        assert!(
            ExternalOtelConfig::resolve_with(
                env(&[
                    ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                    ("OTEL_LOGS_EXPORTER", "otlp"),
                    ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/json"),
                ]),
                None,
            )
            .is_none(),
            "active signal with no override must fail on invalid base"
        );
    }

    #[test]
    fn headers_parsed_and_signal_specific_scoped() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_HEADERS", "x-token=abc, x-org=corp"),
                ("OTEL_EXPORTER_OTLP_LOGS_HEADERS", "x-token=override"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.logs_headers,
            vec![
                ("x-token".to_string(), "override".to_string()),
                ("x-org".to_string(), "corp".to_string()),
            ]
        );
        assert_eq!(
            cfg.metrics_headers,
            vec![
                ("x-token".to_string(), "abc".to_string()),
                ("x-org".to_string(), "corp".to_string()),
            ]
        );
    }

    #[test]
    fn logs_and_metrics_headers_stay_isolated() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_HEADERS", "authorization=Bearer base"),
                (
                    "OTEL_EXPORTER_OTLP_LOGS_HEADERS",
                    "authorization=Bearer logs",
                ),
                (
                    "OTEL_EXPORTER_OTLP_METRICS_HEADERS",
                    "authorization=Bearer metrics",
                ),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.logs_headers,
            vec![("authorization".to_string(), "Bearer logs".to_string())]
        );
        assert_eq!(
            cfg.metrics_headers,
            vec![("authorization".to_string(), "Bearer metrics".to_string())]
        );
    }

    #[test]
    fn ca_certificate_resolved_with_signal_overrides() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_CERTIFICATE", "/etc/ssl/corp-ca.pem"),
                (
                    "OTEL_EXPORTER_OTLP_METRICS_CERTIFICATE",
                    "/etc/ssl/metrics-ca.pem",
                ),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.logs_ca_certificate.as_deref(),
            Some("/etc/ssl/corp-ca.pem")
        );
        assert_eq!(
            cfg.metrics_ca_certificate.as_deref(),
            Some("/etc/ssl/metrics-ca.pem")
        );
    }

    #[test]
    fn ca_certificate_defaults_to_none() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.logs_ca_certificate, None);
        assert_eq!(cfg.metrics_ca_certificate, None);
    }

    #[test]
    fn content_gates_default_off_env_enables() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_LOG_USER_PROMPTS", "1"),
                ("OTEL_LOG_TOOL_DETAILS", "true"),
            ]),
            None,
        )
        .unwrap();
        assert!(cfg.gates.log_user_prompts);
        assert!(cfg.gates.log_tool_details);
    }

    #[test]
    fn intervals_and_timeout_parsed_with_blrp_precedence() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_TIMEOUT", "2500"),
                ("OTEL_METRIC_EXPORT_INTERVAL", "30000"),
                ("OTEL_BLRP_SCHEDULE_DELAY", "1000"),
                ("OTEL_LOGS_EXPORT_INTERVAL", "9999"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.timeout, Duration::from_millis(2500));
        assert_eq!(cfg.metric_export_interval, Duration::from_millis(30_000));
        // Spec name wins over the compatibility alias.
        assert_eq!(cfg.logs_export_interval, Duration::from_millis(1000));
    }

    #[test]
    fn logs_export_interval_alias_honored_when_spec_name_absent() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_LOGS_EXPORT_INTERVAL", "9999"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.logs_export_interval, Duration::from_millis(9999));
    }

    #[test]
    fn file_config_layered_under_env() {
        let file = ExternalOtelFileConfig {
            enabled: Some(true),
            metrics_exporter: Some("otlp".into()),
            logs_exporter: Some("console".into()),
            endpoint: Some("https://file.example:4318".into()),
            log_user_prompts: Some(true),
            log_tool_details: None,
            protocol: None,
            ..Default::default()
        };
        // No env at all: file config alone activates.
        let cfg = ExternalOtelConfig::resolve_with(env(&[]), Some(&file)).unwrap();
        assert_eq!(cfg.metrics_exporter, ExporterSelection::Otlp);
        assert_eq!(cfg.logs_exporter, ExporterSelection::Console);
        assert_eq!(cfg.metrics_endpoint, "https://file.example:4318/v1/metrics");
        assert!(cfg.gates.log_user_prompts);

        // Env wins over file on every layered key.
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("OTEL_METRICS_EXPORTER", "none"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_LOG_USER_PROMPTS", "0"),
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "https://env.example:4318"),
            ]),
            Some(&file),
        )
        .unwrap();
        assert_eq!(cfg.metrics_exporter, ExporterSelection::None);
        assert_eq!(cfg.logs_exporter, ExporterSelection::Otlp);
        assert_eq!(cfg.logs_endpoint, "https://env.example:4318/v1/logs");
        assert!(!cfg.gates.log_user_prompts);

        // Env master switch off wins over file `enabled = true`.
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[("ECHO_AGENT_EXTERNAL_OTEL", "0")]),
            Some(&file),
        );
        assert!(cfg.is_none());
    }

    #[test]
    fn cumulative_temporality_honored() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                (
                    "OTEL_EXPORTER_OTLP_METRICS_TEMPORALITY_PREFERENCE",
                    "cumulative",
                ),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.temporality, TemporalityPreference::Cumulative);
    }

    #[test]
    fn client_identity_defaults_to_none() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.logs_client_certificate, None);
        assert_eq!(cfg.logs_client_key, None);
        assert_eq!(cfg.metrics_client_certificate, None);
        assert_eq!(cfg.metrics_client_key, None);
    }

    #[test]
    fn client_identity_base_vars_apply_to_both_signals() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                (
                    "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
                    "/etc/ssl/client.crt",
                ),
                ("OTEL_EXPORTER_OTLP_CLIENT_KEY", "/etc/ssl/client.key"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.logs_client_certificate.as_deref(),
            Some("/etc/ssl/client.crt")
        );
        assert_eq!(cfg.logs_client_key.as_deref(), Some("/etc/ssl/client.key"));
        assert_eq!(
            cfg.metrics_client_certificate.as_deref(),
            Some("/etc/ssl/client.crt")
        );
        assert_eq!(
            cfg.metrics_client_key.as_deref(),
            Some("/etc/ssl/client.key")
        );
    }

    #[test]
    fn client_identity_per_signal_overrides_isolate() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE", "/etc/ssl/base.crt"),
                ("OTEL_EXPORTER_OTLP_CLIENT_KEY", "/etc/ssl/base.key"),
                (
                    "OTEL_EXPORTER_OTLP_METRICS_CLIENT_CERTIFICATE",
                    "/etc/ssl/metrics.crt",
                ),
                (
                    "OTEL_EXPORTER_OTLP_METRICS_CLIENT_KEY",
                    "/etc/ssl/metrics.key",
                ),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.logs_client_certificate.as_deref(),
            Some("/etc/ssl/base.crt")
        );
        assert_eq!(cfg.logs_client_key.as_deref(), Some("/etc/ssl/base.key"));
        assert_eq!(
            cfg.metrics_client_certificate.as_deref(),
            Some("/etc/ssl/metrics.crt")
        );
        assert_eq!(
            cfg.metrics_client_key.as_deref(),
            Some("/etc/ssl/metrics.key")
        );
    }

    #[test]
    fn client_identity_half_config_clears_both() {
        let cert_only = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                (
                    "OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE",
                    "/etc/ssl/client.crt",
                ),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cert_only.logs_client_certificate, None);
        assert_eq!(cert_only.logs_client_key, None);

        let key_only = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_CLIENT_KEY", "/etc/ssl/client.key"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(key_only.metrics_client_certificate, None);
        assert_eq!(key_only.metrics_client_key, None);
    }

    #[test]
    fn client_identity_signal_override_pair_without_base() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                (
                    "OTEL_EXPORTER_OTLP_LOGS_CLIENT_CERTIFICATE",
                    "/etc/ssl/logs.crt",
                ),
                ("OTEL_EXPORTER_OTLP_LOGS_CLIENT_KEY", "/etc/ssl/logs.key"),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(
            cfg.logs_client_certificate.as_deref(),
            Some("/etc/ssl/logs.crt")
        );
        assert_eq!(cfg.logs_client_key.as_deref(), Some("/etc/ssl/logs.key"));
        assert_eq!(cfg.metrics_client_certificate, None);
        assert_eq!(cfg.metrics_client_key, None);
    }

    #[test]
    fn file_layer_certificate_and_client_identity_paths() {
        let file = ExternalOtelFileConfig {
            enabled: Some(true),
            logs_exporter: Some("otlp".into()),
            metrics_exporter: Some("otlp".into()),
            certificate: Some("/etc/ssl/corp-ca.pem".into()),
            client_certificate: Some("/etc/ssl/client.crt".into()),
            client_key: Some("/etc/ssl/client.key".into()),
            ..Default::default()
        };
        let cfg = ExternalOtelConfig::resolve_with(env(&[]), Some(&file)).unwrap();
        assert_eq!(
            cfg.logs_ca_certificate.as_deref(),
            Some("/etc/ssl/corp-ca.pem")
        );
        assert_eq!(
            cfg.metrics_ca_certificate.as_deref(),
            Some("/etc/ssl/corp-ca.pem")
        );
        assert_eq!(
            cfg.logs_client_certificate.as_deref(),
            Some("/etc/ssl/client.crt")
        );
        assert_eq!(cfg.logs_client_key.as_deref(), Some("/etc/ssl/client.key"));
        assert_eq!(
            cfg.metrics_client_certificate.as_deref(),
            Some("/etc/ssl/client.crt")
        );
        assert_eq!(
            cfg.metrics_client_key.as_deref(),
            Some("/etc/ssl/client.key")
        );
    }

    #[test]
    fn env_client_identity_overrides_file_layer() {
        let file = ExternalOtelFileConfig {
            enabled: Some(true),
            logs_exporter: Some("otlp".into()),
            client_certificate: Some("/file/client.crt".into()),
            client_key: Some("/file/client.key".into()),
            certificate: Some("/file/ca.pem".into()),
            ..Default::default()
        };
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE", "/env/client.crt"),
                ("OTEL_EXPORTER_OTLP_CLIENT_KEY", "/env/client.key"),
                ("OTEL_EXPORTER_OTLP_CERTIFICATE", "/env/ca.pem"),
            ]),
            Some(&file),
        )
        .unwrap();
        assert_eq!(
            cfg.logs_client_certificate.as_deref(),
            Some("/env/client.crt")
        );
        assert_eq!(cfg.logs_client_key.as_deref(), Some("/env/client.key"));
        assert_eq!(cfg.logs_ca_certificate.as_deref(), Some("/env/ca.pem"));
    }

    #[test]
    fn half_env_identity_does_not_cross_with_file_pair() {
        let file = ExternalOtelFileConfig {
            enabled: Some(true),
            logs_exporter: Some("otlp".into()),
            metrics_exporter: Some("otlp".into()),
            client_certificate: Some("/file/client.crt".into()),
            client_key: Some("/file/client.key".into()),
            ..Default::default()
        };

        let cert_only = ExternalOtelConfig::resolve_with(
            env(&[("OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE", "/env/client.crt")]),
            Some(&file),
        )
        .unwrap();
        assert_eq!(cert_only.logs_client_certificate, None);
        assert_eq!(cert_only.logs_client_key, None);
        assert_eq!(cert_only.metrics_client_certificate, None);
        assert_eq!(cert_only.metrics_client_key, None);

        let key_only = ExternalOtelConfig::resolve_with(
            env(&[("OTEL_EXPORTER_OTLP_CLIENT_KEY", "/env/client.key")]),
            Some(&file),
        )
        .unwrap();
        assert_eq!(key_only.logs_client_certificate, None);
        assert_eq!(key_only.logs_client_key, None);
        assert_eq!(key_only.metrics_client_certificate, None);
        assert_eq!(key_only.metrics_client_key, None);
    }

    #[test]
    fn half_signal_env_identity_does_not_cross_with_base_pair() {
        let cfg = ExternalOtelConfig::resolve_with(
            env(&[
                ("ECHO_AGENT_EXTERNAL_OTEL", "1"),
                ("OTEL_LOGS_EXPORTER", "otlp"),
                ("OTEL_METRICS_EXPORTER", "otlp"),
                ("OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE", "/base/client.crt"),
                ("OTEL_EXPORTER_OTLP_CLIENT_KEY", "/base/client.key"),
                (
                    "OTEL_EXPORTER_OTLP_LOGS_CLIENT_CERTIFICATE",
                    "/logs/only.crt",
                ),
            ]),
            None,
        )
        .unwrap();
        assert_eq!(cfg.logs_client_certificate, None);
        assert_eq!(cfg.logs_client_key, None);
        assert_eq!(
            cfg.metrics_client_certificate.as_deref(),
            Some("/base/client.crt")
        );
        assert_eq!(cfg.metrics_client_key.as_deref(), Some("/base/client.key"));
    }
}
