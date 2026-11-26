//! Opt-in, content-redacted **external OTEL** telemetry stream.
//!
//! Enterprise customers point the EchoAgent CLI at *their own* OpenTelemetry
//! collector (standard `OTEL_*` env vars + the `ECHO_AGENT_EXTERNAL_OTEL` master
//! switch) and receive a curated, ZDR-safe schema: ~6 counters and ~17
//! log-record events fanned out from the same typed call sites that emit the
//! product events ([`crate::session_ctx::log_event`]).
//!
//! Structural invariants (enforced by construction and tests):
//! - The providers here are **never** registered with `opentelemetry::global`
//!   (the internal tracer provider owns the global slot); everything is
//!   handle-based through the [`EXTERNAL`] registry.
//! - The exporters carry **only** customer headers/metadata from
//!   `OTEL_EXPORTER_OTLP_HEADERS` — this module has no dependency on
//!   `AuthCredentialProvider` and no code path that can attach internal auth
//!   headers.
//! - Default **off**: with `ECHO_AGENT_EXTERNAL_OTEL` unset (or no exporter
//!   selected) nothing is constructed — zero allocation, zero threads, zero
//!   sockets.
//! - Independent of `TelemetryMode`, GCS trace upload, and the data-collection /
//!   data-retention opt-outs (user-confirmed): those govern EchoAgent-side
//!   retention; this stream ships only to the customer's own collector under
//!   the customer's own explicit double opt-in.
//!
//! This module is the second authoritative privacy boundary in this crate
//! (alongside `otel_layer::redact`).

pub mod config;
mod emit;
pub(crate) mod providers;
mod redact;
pub mod schema;
pub mod truncate;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use opentelemetry::logs::LoggerProvider as _;
use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::logs::{SdkLogger, SdkLoggerProvider};
use opentelemetry_sdk::metrics::SdkMeterProvider;

pub use config::{ContentGates, ExternalOtelConfig, ExternalOtelFileConfig};

static EXTERNAL: OnceLock<Option<Arc<ExternalTelemetry>>> = OnceLock::new();

/// Identity *attributes* (plain id strings — never tokens). Derived from a
/// `CredentialSnapshot` at the telemetry-client init sites; updated post-auth
/// and on logout.
#[derive(Debug, Clone, Default)]
pub struct IdentityAttrs {
    pub user_id: Option<String>,
    pub organization_id: Option<String>,
    pub team_id: Option<String>,
    pub deployment_id: Option<String>,
}

impl IdentityAttrs {
    pub fn from_snapshot(snapshot: &echo_agent_auth::CredentialSnapshot) -> Self {
        Self {
            user_id: snapshot.user_id.clone(),
            organization_id: snapshot.organization_id.clone(),
            team_id: snapshot.team_id.clone(),
            deployment_id: snapshot.deployment_id.clone(),
        }
    }
}

/// Remote-settings policy for the external stream. **Restrictive-only by
/// construction**: there is deliberately no enable direction (remote settings
/// are fetched per-run and never persisted, so a remote "enable" could never
/// reach init).
#[derive(Debug, Clone, Copy, Default)]
pub struct ExternalOtelRemotePolicy {
    /// Remote-policy force-disable: flush, then drop subsequent emissions in-process.
    pub force_disable: bool,
    /// Force the content gates off regardless of local env/config.
    pub lock_content_gates: bool,
}

/// The handle owning both providers. Never global; reached only through the
/// [`EXTERNAL`] registry.
pub struct ExternalTelemetry {
    logger_provider: Option<SdkLoggerProvider>,
    meter_provider: Option<SdkMeterProvider>,
    logger: Option<SdkLogger>,
    instruments: Option<emit::Instruments>,
    /// Emission gate; cleared by the remote force-disable. The single
    /// authority for "emitting right now".
    active: AtomicBool,
    /// Content gates; may only TIGHTEN post-init.
    gates: redact::SharedGates,
    identity: parking_lot::RwLock<IdentityAttrs>,
    /// `event.sequence` (monotonic, per-process).
    sequence: AtomicU64,
    shutdown_once: std::sync::Once,
    include_session_id_on_metrics: bool,
    include_version_on_metrics: bool,
    app_version: String,
    health: Arc<redact::ExportHealth>,
    /// True when any signal was configured with a client cert/key pair.
    mtls_identity_configured: bool,
    /// Init summary for the adoption meta-event (emitted once, post-auth).
    configured_meta: ConfiguredMeta,
    meta_event_once: std::sync::Once,
}

/// Snapshot of external-stream export-health counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExportHealthSnapshot {
    pub records_dropped: u64,
    pub metric_exports_dropped: u64,
    pub export_failures: u64,
    pub export_successes: u64,
}

#[derive(Debug, Clone)]
struct ConfiguredMeta {
    metrics_exporter: &'static str,
    logs_exporter: &'static str,
    logs_endpoint_origin: String,
    metrics_endpoint_origin: String,
    protocol: String,
    prompts_gate: bool,
    details_gate: bool,
    source: &'static str,
}

fn exporter_label(sel: config::ExporterSelection) -> &'static str {
    match sel {
        config::ExporterSelection::None => "none",
        config::ExporterSelection::Otlp => "otlp",
        config::ExporterSelection::Console => "console",
    }
}

impl ExternalTelemetry {
    pub(crate) fn next_sequence(&self) -> u64 {
        self.sequence.fetch_add(1, Ordering::Relaxed)
    }
}

/// Initialize the external stream. Called once from binary startup after
/// config resolution, **before auth** (no credentials needed). `None` records
/// the dormant state — the default path allocates nothing.
pub fn init(cfg: Option<ExternalOtelConfig>) {
    let value = cfg.and_then(build_handle);
    if EXTERNAL.set(value).is_err() {
        tracing::debug!("external otel: init called more than once; keeping first registration");
    }
}

fn build_handle(cfg: ExternalOtelConfig) -> Option<Arc<ExternalTelemetry>> {
    // No-double-send invariant, enforced in code (not release discipline):
    // if the internal firehose resolved its endpoint/headers from
    // `OTEL_EXPORTER_OTLP_*` (the deprecated fallback), refuse to activate.
    if cfg.internal_pipeline_consumed_otel_vars {
        tracing::warn!(
            "external otel: refusing to activate — the internal trace pipeline consumed \
             OTEL_EXPORTER_OTLP_* (deprecated fallback). Migrate internal repointing to \
             ECHO_AGENT_INTERNAL_OTLP_* to use the external stream."
        );
        return None;
    }

    let gates: redact::SharedGates = Arc::new(parking_lot::RwLock::new(cfg.gates));
    let health = Arc::new(redact::ExportHealth::default());
    let built = match providers::build(&cfg, gates.clone(), health.clone()) {
        Ok(built) => built,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "external otel: exporter construction failed; stream disabled"
            );
            return None;
        }
    };
    if built.logger_provider.is_none() && built.meter_provider.is_none() {
        return None;
    }

    let logger = built
        .logger_provider
        .as_ref()
        .map(|p| p.logger(schema::SCOPE_NAME));
    let instruments = built
        .meter_provider
        .as_ref()
        .map(|p| emit::Instruments::new(&p.meter(schema::SCOPE_NAME)));

    let configured_meta = ConfiguredMeta {
        metrics_exporter: exporter_label(cfg.metrics_exporter),
        logs_exporter: exporter_label(cfg.logs_exporter),
        logs_endpoint_origin: crate::redact_common::url_origin(&cfg.logs_endpoint).into_owned(),
        metrics_endpoint_origin: crate::redact_common::url_origin(&cfg.metrics_endpoint)
            .into_owned(),
        protocol: if cfg.logs_transport == cfg.metrics_transport {
            cfg.logs_transport.as_protocol_str().to_string()
        } else {
            format!(
                "logs={},metrics={}",
                cfg.logs_transport.as_protocol_str(),
                cfg.metrics_transport.as_protocol_str()
            )
        },
        prompts_gate: cfg.gates.log_user_prompts,
        details_gate: cfg.gates.log_tool_details,
        source: cfg.enabled_source,
    };

    let mtls_identity_configured = cfg.logs_client_certificate.is_some()
        || cfg.logs_client_key.is_some()
        || cfg.metrics_client_certificate.is_some()
        || cfg.metrics_client_key.is_some();

    tracing::debug!(
        metrics_exporter = configured_meta.metrics_exporter,
        logs_exporter = configured_meta.logs_exporter,
        "external otel: stream active"
    );

    Some(Arc::new(ExternalTelemetry {
        logger_provider: built.logger_provider,
        meter_provider: built.meter_provider,
        logger,
        instruments,
        active: AtomicBool::new(true),
        gates,
        identity: parking_lot::RwLock::new(IdentityAttrs::default()),
        sequence: AtomicU64::new(0),
        shutdown_once: std::sync::Once::new(),
        include_session_id_on_metrics: cfg.include_session_id_on_metrics,
        include_version_on_metrics: cfg.include_version_on_metrics,
        app_version: cfg.client.client_version.clone(),
        health,
        mtls_identity_configured,
        configured_meta,
        meta_event_once: std::sync::Once::new(),
    }))
}

fn handle() -> Option<Arc<ExternalTelemetry>> {
    EXTERNAL.get().and_then(|opt| opt.clone())
}

fn active_handle() -> Option<Arc<ExternalTelemetry>> {
    handle().filter(|ext| ext.active.load(Ordering::Relaxed))
}

/// Fail-closed OTEL gate. Defaults open; the leader closes it before init and
/// re-opens it when settings resolve.
///
/// Opening is the synchronizing event: `OtelGate::apply_and_open` applies the
/// remote force-disable (`active = false`) and then opens here, so an emitter
/// whose `Acquire` read observes the `Release` open also observes
/// `active = false`; the emit-path `active` load can therefore stay `Relaxed`.
/// The window-expiry open has no such pairing and relies on eventual
/// visibility, acceptable because the policy is tighten-only.
static SETTINGS_RESOLVED: AtomicBool = AtomicBool::new(true);

const DEFAULT_SETTINGS_GATE_MAX_WAIT: Duration = Duration::from_secs(30);

static SETTINGS_GATE_MAX_WAIT_MS: AtomicU64 =
    AtomicU64::new(DEFAULT_SETTINGS_GATE_MAX_WAIT.as_millis() as u64);

static GATE_CLOSED_AT_MS: AtomicU64 = AtomicU64::new(0);

fn process_uptime_ms() -> u64 {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    u64::try_from(
        START
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

/// Set the bound on the fail-closed window.
pub fn set_settings_gate_max_wait(max_wait: Duration) {
    SETTINGS_GATE_MAX_WAIT_MS.store(
        u64::try_from(max_wait.as_millis()).unwrap_or(u64::MAX),
        Ordering::Relaxed,
    );
}

/// The current bound on the fail-closed window.
pub fn settings_gate_max_wait() -> Duration {
    Duration::from_millis(SETTINGS_GATE_MAX_WAIT_MS.load(Ordering::Relaxed))
}

/// Close the gate (leader preinit + account switch).
pub fn suppress_external_otel_until_settings() {
    GATE_CLOSED_AT_MS.store(process_uptime_ms(), Ordering::Relaxed);
    // `Release`: a reader that observes the close must also observe the
    // timestamp published just above, or it would measure this window from an
    // earlier close and open immediately.
    SETTINGS_RESOLVED.store(false, Ordering::Release);
}

/// Open the gate. `Release` publishes the force-disable applied just before it.
pub fn mark_external_otel_settings_resolved() {
    if !SETTINGS_RESOLVED.swap(true, Ordering::Release) {
        tracing::debug!("external otel: settings resolved, emission gate opened");
    }
}

/// Read the gate. `Acquire` pairs with the `Release` open (and with the
/// `Release` close that publishes the window start).
#[inline]
pub fn is_settings_gate_open() -> bool {
    SETTINGS_RESOLVED.load(Ordering::Acquire) || settings_gate_window_expired()
}

#[cold]
fn settings_gate_window_expired() -> bool {
    let waited = process_uptime_ms().saturating_sub(GATE_CLOSED_AT_MS.load(Ordering::Relaxed));
    if waited < SETTINGS_GATE_MAX_WAIT_MS.load(Ordering::Relaxed) {
        return false;
    }
    static LOGGED: std::sync::Once = std::sync::Once::new();
    LOGGED.call_once(|| {
        tracing::warn!(
            waited_ms = waited,
            "external otel: no fleet policy arrived within the bounded window; \
             emitting under local configuration (a policy that arrives later still applies)"
        );
    });
    true
}

/// Cheap check used by the fan-out hook and the split-sink call sites:
/// registry present AND the runtime emission gate set AND the settings gate
/// open. A stale `true` read only costs a wasted mapping, never an export
/// ([`emit`] re-checks).
pub fn is_active() -> bool {
    is_settings_gate_open()
        && matches!(EXTERNAL.get(), Some(Some(ext)) if ext.active.load(Ordering::Relaxed))
}

/// Map and emit one typed telemetry event. No-op unless the stream is active
/// and the event has an `external = …` mapping. Synchronous and cheap (the
/// batch processor queues; nothing blocks on I/O).
pub fn emit<T: crate::events::TelemetryEvent>(data: &T) {
    // Fail-closed: suppress until the leader confirms the remote policy; open by
    // default for everyone else.
    if !is_settings_gate_open() {
        return;
    }
    let Some(ext) = active_handle() else {
        return;
    };
    let Some(record) = data.external_record() else {
        return;
    };
    emit::emit_record(&ext, record);
}

/// Update identity attrs when auth completes (called alongside the
/// telemetry-client init sites). Also emits the one-shot internal adoption
/// meta-event — post-auth, when the product events client is live.
pub fn set_identity(attrs: IdentityAttrs) {
    let Some(ext) = handle() else {
        return;
    };
    set_identity_on(&ext, attrs);
}

pub(crate) fn set_identity_on(ext: &ExternalTelemetry, attrs: IdentityAttrs) {
    *ext.identity.write() = attrs;
    ext.meta_event_once.call_once(|| {
        let meta = &ext.configured_meta;
        crate::session_ctx::log_session_event(crate::events::ExternalOtelConfigured {
            metrics_exporter: meta.metrics_exporter.to_owned(),
            logs_exporter: meta.logs_exporter.to_owned(),
            protocol: meta.protocol.to_owned(),
            logs_endpoint_origin: meta.logs_endpoint_origin.clone(),
            metrics_endpoint_origin: meta.metrics_endpoint_origin.clone(),
            prompts_gate: meta.prompts_gate,
            details_gate: meta.details_gate,
            source: meta.source.to_owned(),
        });
    });
}

/// Apply remote policy when `RemoteSettings` arrive (post-auth, alongside
/// [`set_identity`]). **TIGHTEN-ONLY**: may clear `active` (remote-policy
/// force-disable, flushes then drops subsequent emissions) and may force content gates
/// off; it can never enable a stream that env/config left off, and never
/// loosens gates mid-run.
pub fn apply_remote_policy(policy: ExternalOtelRemotePolicy) {
    let Some(ext) = handle() else {
        return;
    };
    apply_remote_policy_on(&ext, policy);
}

pub(crate) fn apply_remote_policy_on(ext: &ExternalTelemetry, policy: ExternalOtelRemotePolicy) {
    if policy.lock_content_gates {
        let mut gates = ext.gates.write();
        if gates.log_user_prompts || gates.log_tool_details {
            *gates = ContentGates::default();
            drop(gates);
            crate::session_ctx::log_session_event(crate::events::ExternalOtelRemotePolicyApplied {
                action: "gates_locked".to_owned(),
            });
        }
    }
    if policy.force_disable && ext.active.swap(false, Ordering::Relaxed) {
        flush_on(ext);
        crate::session_ctx::log_session_event(crate::events::ExternalOtelRemotePolicyApplied {
            action: "force_disable".to_owned(),
        });
        tracing::debug!("external otel: force-disabled by remote policy");
    }
}

/// Flush both providers (logout path: called *before* credentials are
/// cleared, so post-logout records cannot carry the prior user's ids —
/// follow with [`set_identity`] carrying the new/empty identity).
pub fn flush() {
    let Some(ext) = handle() else {
        return;
    };
    flush_on(&ext);
}

pub(crate) fn flush_on(ext: &ExternalTelemetry) {
    if let Some(p) = ext.logger_provider.as_ref()
        && let Err(e) = p.force_flush()
    {
        tracing::debug!(error = %e, "external otel: logger flush failed");
    }
    if let Some(p) = ext.meter_provider.as_ref()
        && let Err(e) = p.force_flush()
    {
        tracing::debug!(error = %e, "external otel: meter flush failed");
    }
}

/// Flush + shutdown both providers with a 2-second watchdog. Idempotent —
/// reachable from every `shutdown_otel()` exit path (16 `OtelGuard` sites,
/// the direct call, and the signal handler); subsequent calls are no-ops.
pub fn shutdown() {
    let Some(ext) = handle() else {
        return;
    };
    ext.shutdown_once.call_once(|| {
        ext.active.store(false, Ordering::Relaxed);
        let logger_provider = ext.logger_provider.clone();
        let meter_provider = ext.meter_provider.clone();
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        // Detached thread + timed wait: a hung provider must not hang exit
        // (`std::thread::scope` is unusable here — it joins unconditionally).
        std::thread::spawn(move || {
            if let Some(p) = logger_provider
                && let Err(e) = p.shutdown()
            {
                tracing::debug!(error = %e, "external otel: logger shutdown failed");
            }
            if let Some(p) = meter_provider
                && let Err(e) = p.shutdown()
            {
                tracing::debug!(error = %e, "external otel: meter shutdown failed");
            }
            let _ = tx.send(());
        });
        if rx.recv_timeout(std::time::Duration::from_secs(2)).is_err() {
            tracing::debug!("external otel: shutdown watchdog expired; abandoning flush thread");
        }
        // After provider shutdown (which flushes pending batches). Short-lived
        // CLI exits often only export on this path, so health counters and the
        // mTLS total-failure warn must run after it — not before.
        emit_export_health(&ext);
    });
}

/// Best-effort product-events export-health meta-event (never exported
/// externally — avoid feedback loops). Emitting needs a Tokio runtime
/// (`emit_event` spawns); skip silently when exiting without one.
fn emit_export_health(ext: &ExternalTelemetry) {
    let health = &ext.health;
    let snapshot = crate::events::ExternalOtelExportHealth {
        records_dropped: health.records_dropped.load(Ordering::Relaxed),
        metric_exports_dropped: health.metric_exports_dropped.load(Ordering::Relaxed),
        export_failures: health.export_failures.load(Ordering::Relaxed),
        export_successes: health.export_successes.load(Ordering::Relaxed),
    };
    tracing::debug!(
        records_dropped = snapshot.records_dropped,
        metric_exports_dropped = snapshot.metric_exports_dropped,
        export_failures = snapshot.export_failures,
        export_successes = snapshot.export_successes,
        "external otel: export health"
    );
    // mTLS misconfig (expired/wrong CA client cert) otherwise looks like a
    // clean exit with an empty collector. Surface that at warn once on
    // shutdown when every export attempt failed.
    if ext.mtls_identity_configured
        && snapshot.export_failures > 0
        && snapshot.export_successes == 0
    {
        let last_error = health
            .last_export_error
            .lock()
            .clone()
            .map(|e| crate::redact_common::redact_urls_in_text(&e))
            .unwrap_or_else(|| "unknown".into());
        tracing::warn!(
            logs_endpoint_origin = %ext.configured_meta.logs_endpoint_origin,
            metrics_endpoint_origin = %ext.configured_meta.metrics_endpoint_origin,
            export_failures = snapshot.export_failures,
            last_error = %last_error,
            "external otel: mTLS identity configured but every export failed"
        );
    }
    if tokio::runtime::Handle::try_current().is_ok() {
        crate::session_ctx::log_session_event(snapshot);
    }
}

/// Snapshot of export-health counters for the active external stream.
/// `None` when the stream was never activated.
pub fn export_health() -> Option<ExportHealthSnapshot> {
    handle().map(|ext| ExportHealthSnapshot {
        records_dropped: ext.health.records_dropped.load(Ordering::Relaxed),
        metric_exports_dropped: ext.health.metric_exports_dropped.load(Ordering::Relaxed),
        export_failures: ext.health.export_failures.load(Ordering::Relaxed),
        export_successes: ext.health.export_successes.load(Ordering::Relaxed),
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Build an [`ExternalTelemetry`] over in-memory exporters so unit tests
    //! can assert exactly what would reach the wire (post-validator).

    use super::*;
    use opentelemetry_sdk::logs::InMemoryLogExporter;
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader};

    pub(crate) struct TestStream {
        pub ext: ExternalTelemetry,
        pub logs: InMemoryLogExporter,
        pub metrics: InMemoryMetricExporter,
    }

    pub(crate) fn build(gates: ContentGates) -> TestStream {
        let shared_gates: redact::SharedGates = Arc::new(parking_lot::RwLock::new(gates));
        let health = Arc::new(redact::ExportHealth::default());
        let logs = InMemoryLogExporter::default();
        let metrics = InMemoryMetricExporter::default();

        let logger_provider = SdkLoggerProvider::builder()
            .with_simple_exporter(redact::RedactingLogExporter::new(
                logs.clone(),
                shared_gates.clone(),
                health.clone(),
            ))
            .build();
        let meter_provider = SdkMeterProvider::builder()
            .with_reader(
                PeriodicReader::builder(redact::ValidatingMetricExporter::new(
                    metrics.clone(),
                    health.clone(),
                ))
                .build(),
            )
            .build();

        let logger = logger_provider.logger(schema::SCOPE_NAME);
        let instruments = emit::Instruments::new(&meter_provider.meter(schema::SCOPE_NAME));

        let ext = ExternalTelemetry {
            logger_provider: Some(logger_provider),
            meter_provider: Some(meter_provider),
            logger: Some(logger),
            instruments: Some(instruments),
            active: AtomicBool::new(true),
            gates: shared_gates,
            identity: parking_lot::RwLock::new(IdentityAttrs::default()),
            sequence: AtomicU64::new(0),
            shutdown_once: std::sync::Once::new(),
            include_session_id_on_metrics: true,
            include_version_on_metrics: false,
            app_version: String::new(),
            health,
            mtls_identity_configured: false,
            configured_meta: ConfiguredMeta {
                metrics_exporter: "test",
                logs_exporter: "test",
                logs_endpoint_origin: String::new(),
                metrics_endpoint_origin: String::new(),
                protocol: "test".into(),
                prompts_gate: gates.log_user_prompts,
                details_gate: gates.log_tool_details,
                source: "env",
            },
            meta_event_once: std::sync::Once::new(),
        };
        TestStream { ext, logs, metrics }
    }

    pub(crate) fn emit_into(stream: &TestStream, record: schema::ExternalRecord) {
        emit::emit_record(&stream.ext, record);
        stream
            .ext
            .logger_provider
            .as_ref()
            .expect("test logger provider")
            .force_flush()
            .expect("flush logs");
        stream
            .ext
            .meter_provider
            .as_ref()
            .expect("test meter provider")
            .force_flush()
            .expect("flush metrics");
    }

    pub(crate) fn emit_event_into<T: crate::events::TelemetryEvent>(
        stream: &TestStream,
        event: &T,
    ) {
        if let Some(record) = event.external_record() {
            emit_into(stream, record);
        }
    }
}

#[cfg(test)]
mod tests;
