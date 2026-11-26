//! Export-time fail-closed validators for the external stream.
//!
//! The primary redaction (typed-key schema, gating, secret scrub, truncation)
//! happens at **emit time** in [`super::emit`], because `opentelemetry_sdk`
//! 0.30 log records and metric data are not mutable from an exporter wrapper.
//! These wrappers are the **authoritative chokepoint** anyway: they verify,
//! per record/data point, that nothing reaches the wire that the emit path
//! shouldn't have produced — and on any violation they *drop* (a record for
//! logs, the whole export for metrics) rather than scrub in place. Dropping
//! telemetry on a schema bug is acceptable; leaking is not.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use opentelemetry::InstrumentationScope;
use opentelemetry::logs::AnyValue;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::logs::{LogBatch, LogExporter, SdkLogRecord};
use opentelemetry_sdk::metrics::Temporality;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::exporter::PushMetricExporter;

use super::config::ContentGates;
use super::schema::{Gate, external_allowed_keys, gate_for_key};

/// Shared, tighten-only view of the content gates. The remote kill switch may
/// force gates off mid-run; the exporters re-read on every export.
pub(crate) type SharedGates = Arc<parking_lot::RwLock<ContentGates>>;

/// Export-health counters (read by the internal `export_health` meta-event).
#[derive(Debug, Default)]
pub(crate) struct ExportHealth {
    /// Log records dropped by the validator.
    pub records_dropped: AtomicU64,
    /// Whole metric exports dropped by the validator.
    pub metric_exports_dropped: AtomicU64,
    /// Failed export attempts (transport errors), both signals.
    pub export_failures: AtomicU64,
    /// Successful export attempts, both signals.
    pub export_successes: AtomicU64,
    /// Most recent transport error message (for shutdown-time diagnostics).
    pub last_export_error: parking_lot::Mutex<Option<String>>,
}

fn gate_open(gates: &ContentGates, gate: Gate) -> bool {
    match gate {
        Gate::UserPrompts => gates.log_user_prompts,
        Gate::ToolDetails => gates.log_tool_details,
    }
}

/// `true` when this record is clean: every attribute key is schema-named,
/// gated keys have their gate open, string values carry no secret shapes the
/// emit path should have scrubbed, and the body is empty (`event.name` is the
/// structured identity — external records carry no free-text body).
fn record_is_clean(record: &SdkLogRecord, gates: &ContentGates) -> bool {
    if record.body().is_some() {
        tracing::debug!("external otel: dropping record with non-empty body");
        return false;
    }
    for (key, value) in record.attributes_iter() {
        let key_str = key.as_str();
        if !external_allowed_keys().contains(key_str) {
            tracing::debug!(
                key = key_str,
                "external otel: dropping record with non-schema key"
            );
            return false;
        }
        if let Some(gate) = gate_for_key(key_str)
            && !gate_open(gates, gate)
        {
            tracing::debug!(
                key = key_str,
                "external otel: dropping record with closed-gate key"
            );
            return false;
        }
        match value {
            AnyValue::Int(_) | AnyValue::Double(_) | AnyValue::Boolean(_) => {}
            AnyValue::String(s) => {
                if crate::redact_common::redact_owned(s.as_str()).is_some() {
                    tracing::debug!(
                        key = key_str,
                        "external otel: dropping record with unscrubbed string value"
                    );
                    return false;
                }
            }
            // Bytes / lists / maps / future variants are content (fail-closed).
            _ => {
                tracing::debug!(
                    key = key_str,
                    "external otel: dropping record with non-scalar value"
                );
                return false;
            }
        }
    }
    true
}

/// Wraps the OTLP [`LogExporter`]; drops any record that violates the pinned
/// schema before delegating.
#[derive(Debug)]
pub(crate) struct RedactingLogExporter<E> {
    inner: E,
    gates: SharedGates,
    health: Arc<ExportHealth>,
}

impl<E> RedactingLogExporter<E> {
    pub(crate) fn new(inner: E, gates: SharedGates, health: Arc<ExportHealth>) -> Self {
        Self {
            inner,
            gates,
            health,
        }
    }
}

impl<E: LogExporter> LogExporter for RedactingLogExporter<E> {
    fn export(
        &self,
        batch: LogBatch<'_>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let gates = *self.gates.read();
        async move {
            let clean: Vec<(&SdkLogRecord, &InstrumentationScope)> = batch
                .iter()
                .filter(|(record, _)| {
                    let ok = record_is_clean(record, &gates);
                    if !ok {
                        self.health.records_dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    ok
                })
                .collect();
            if clean.is_empty() {
                return Ok(());
            }
            let result = self.inner.export(LogBatch::new(&clean)).await;
            match &result {
                Ok(()) => {
                    self.health.export_successes.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    self.health.export_failures.fetch_add(1, Ordering::Relaxed);
                    *self.health.last_export_error.lock() = Some(e.to_string());
                }
            };
            result
        }
    }

    fn shutdown_with_timeout(&self, timeout: std::time::Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.inner.set_resource(resource);
    }
}

/// `true` when every data point's attribute keys are within the pinned
/// metric-attribute set.
fn metrics_are_clean(metrics: &ResourceMetrics) -> bool {
    fn keys_ok<'a>(attrs: impl Iterator<Item = &'a opentelemetry::KeyValue>) -> bool {
        for kv in attrs {
            let key = kv.key.as_str();
            if !super::schema::METRIC_ALLOWED_ATTR_KEYS.contains(&key) {
                tracing::debug!(
                    key,
                    "external otel: metric data point carries a non-schema attribute key"
                );
                return false;
            }
        }
        true
    }

    fn data_ok<T>(data: &MetricData<T>) -> bool {
        match data {
            MetricData::Gauge(g) => g.data_points().all(|p| keys_ok(p.attributes())),
            MetricData::Sum(s) => s.data_points().all(|p| keys_ok(p.attributes())),
            MetricData::Histogram(h) => h.data_points().all(|p| keys_ok(p.attributes())),
            MetricData::ExponentialHistogram(h) => h.data_points().all(|p| keys_ok(p.attributes())),
        }
    }

    metrics.scope_metrics().all(|scope| {
        scope.metrics().all(|metric| match metric.data() {
            AggregatedMetrics::F64(d) => data_ok(d),
            AggregatedMetrics::U64(d) => data_ok(d),
            AggregatedMetrics::I64(d) => data_ok(d),
        })
    })
}

/// Wraps the OTLP `MetricExporter`. `opentelemetry_sdk` 0.30's
/// `ResourceMetrics` read path is iterator-based and cannot be mutated, so on
/// any attribute-key violation the wrapper **drops the entire export**
/// (returns `Ok`, logs an internal warning, increments the export-health
/// counter) rather than scrubbing in place. Coarse, but genuinely
/// fail-closed.
#[derive(Debug)]
pub(crate) struct ValidatingMetricExporter<E> {
    inner: E,
    health: Arc<ExportHealth>,
}

impl<E> ValidatingMetricExporter<E> {
    pub(crate) fn new(inner: E, health: Arc<ExportHealth>) -> Self {
        Self { inner, health }
    }
}

impl<E: PushMetricExporter> PushMetricExporter for ValidatingMetricExporter<E> {
    fn export(
        &self,
        metrics: &ResourceMetrics,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let clean = metrics_are_clean(metrics);
        async move {
            if !clean {
                self.health
                    .metric_exports_dropped
                    .fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    "external otel: dropped metric export (schema violation; fail-closed)"
                );
                return Ok(());
            }
            let result = self.inner.export(metrics).await;
            match &result {
                Ok(()) => {
                    self.health.export_successes.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    self.health.export_failures.fetch_add(1, Ordering::Relaxed);
                    *self.health.last_export_error.lock() = Some(e.to_string());
                }
            };
            result
        }
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn shutdown_with_timeout(&self, timeout: std::time::Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn temporality(&self) -> Temporality {
        self.inner.temporality()
    }
}
