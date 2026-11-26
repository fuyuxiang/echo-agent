//! Forward curated `tracing` events to the connected server over the
//! WebSocket transport (`logs.donate`).
//!
//! [`DonatingLogLayer`] is installed **inert** at startup and activated
//! post-connect by swapping in a [`LogDonationSender`] (a global-subscriber
//! constraint); while inert, selected events are dropped before enqueueing.
//!
//! Only events on the [`TELEMETRY_TARGET`] target at `>= INFO` are
//! forwarded, and only fields in [`ALLOWED_FIELDS`] are included; other
//! fields such as `reason`/`error` are omitted.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use arc_swap::ArcSwapOption;
use base64::Engine as _;
use echo_agent_tool_protocol::{MAX_DONATION_BYTES, MAX_LOG_RECORDS_PER_DONATION};
use fastrace::collector::SpanContext;
use opentelemetry_proto::tonic::collector::logs::v1::ExportLogsServiceRequest;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue, any_value};
use opentelemetry_proto::tonic::logs::v1::{LogRecord, ResourceLogs, ScopeLogs};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prost::Message as _;
use tokio::sync::mpsc;
use tracing::Level;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

use crate::donate_pump::{
    PENDING_FLUSHES, PumpMsg, drain_via, make_resource, now_unix_nanos, run_pump, string_kv,
    string_value,
};
use crate::server::ToolServer;

/// Stable target the workspace routes selected events through.
/// The layer selects exactly this target, ignoring global
/// `RUST_LOG`. The server re-stamps it as the OTLP scope name.
pub const TELEMETRY_TARGET: &str = "workspace::telemetry";

/// Set of forwardable field names — guaranteed-literal or numeric.
/// Only the listed fields are included; other fields such as
/// `reason`/`error`/`object_path`/`gcs_path` are omitted.
const ALLOWED_FIELDS: &[&str] = &[
    "session_id",
    "turn_number",
    "phase",
    "bytes",
    "file_count",
    "pending",
    "pending_bytes",
    "sample_period_secs",
    "error_category",
    "outcome",
    "skip_reason",
    "drain_reason",
    "grace_ms",
    "active_at_start",
    "pending_at_start",
    "producers_at_start",
];

/// Flush a buffered batch once it reaches this many records.
const LOG_BATCH_FLUSH_RECORDS: usize = 32;
/// Flush a partial batch once its oldest record is at least this old
/// (checked on the next event; the tail is fenced by teardown).
const LOG_BATCH_MAX_AGE: Duration = Duration::from_secs(2);

fn is_allowed(name: &str) -> bool {
    ALLOWED_FIELDS.contains(&name)
}

/// `tracing::Level` → OTLP (`SeverityText`, `SeverityNumber`).
fn severity(level: &Level) -> (&'static str, i32) {
    match *level {
        Level::ERROR => ("ERROR", 17),
        Level::WARN => ("WARN", 13),
        Level::INFO => ("INFO", 9),
        Level::DEBUG => ("DEBUG", 5),
        Level::TRACE => ("TRACE", 1),
    }
}

/// `>= INFO` in severity terms (INFO/WARN/ERROR). Note tracing orders
/// `ERROR < WARN < INFO < DEBUG < TRACE`, so this is `level <= INFO`.
fn at_least_info(level: &Level) -> bool {
    *level <= Level::INFO
}

/// Big-endian byte encoding of the local parent's ids into the OTLP
/// 16-byte / 8-byte fields; empty when no fastrace local parent is
/// active (the common case for detached producer tasks).
fn current_ids() -> (Vec<u8>, Vec<u8>) {
    match SpanContext::current_local_parent() {
        Some(ctx) => encode_ids(&ctx),
        None => (Vec::new(), Vec::new()),
    }
}

fn encode_ids(ctx: &SpanContext) -> (Vec<u8>, Vec<u8>) {
    (
        ctx.trace_id.0.to_be_bytes().to_vec(),
        ctx.span_id.0.to_be_bytes().to_vec(),
    )
}

/// Field visitor: keeps the message as the OTLP `Body` and only
/// allowlisted fields as attributes; everything else is dropped.
#[derive(Default)]
struct AllowlistVisitor {
    body: Option<String>,
    attributes: Vec<KeyValue>,
}

impl Visit for AllowlistVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if name == "message" {
            self.body = Some(format!("{value:?}"));
        } else if is_allowed(name) {
            self.attributes.push(string_kv(name, format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let name = field.name();
        if name == "message" {
            self.body = Some(value.to_owned());
        } else if is_allowed(name) {
            self.attributes.push(string_kv(name, value.to_owned()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if is_allowed(field.name()) {
            self.push_int(field.name(), value);
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if is_allowed(field.name()) {
            self.push_int(field.name(), value as i64);
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        if is_allowed(field.name()) {
            self.attributes.push(KeyValue {
                key: field.name().to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::BoolValue(value)),
                }),
                ..Default::default()
            });
        }
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if is_allowed(field.name()) {
            self.attributes.push(KeyValue {
                key: field.name().to_owned(),
                value: Some(AnyValue {
                    value: Some(any_value::Value::DoubleValue(value)),
                }),
                ..Default::default()
            });
        }
    }
}

impl AllowlistVisitor {
    fn push_int(&mut self, name: &str, value: i64) {
        self.attributes.push(KeyValue {
            key: name.to_owned(),
            value: Some(AnyValue {
                value: Some(any_value::Value::IntValue(value)),
            }),
            ..Default::default()
        });
    }
}

fn build_log_record(level: &Level, visitor: AllowlistVisitor) -> LogRecord {
    let now_nanos = now_unix_nanos();
    let (text, number) = severity(level);
    let (trace_id, span_id) = current_ids();
    LogRecord {
        time_unix_nano: now_nanos,
        observed_time_unix_nano: now_nanos,
        severity_number: number,
        severity_text: text.to_owned(),
        body: visitor.body.map(string_value),
        attributes: visitor.attributes,
        trace_id,
        span_id,
        ..Default::default()
    }
}

/// Encodes batches of OTLP `LogRecord`s onto the pump channel. Chunks at
/// [`MAX_LOG_RECORDS_PER_DONATION`], drops payloads over
/// [`MAX_DONATION_BYTES`], and never blocks.
#[derive(Clone)]
struct PumpLogExporter {
    tx: mpsc::Sender<PumpMsg>,
    resource: Resource,
}

impl PumpLogExporter {
    fn export(&self, mut records: Vec<LogRecord>) {
        while !records.is_empty() {
            let chunk = if records.len() > MAX_LOG_RECORDS_PER_DONATION {
                let rest = records.split_off(MAX_LOG_RECORDS_PER_DONATION);
                std::mem::replace(&mut records, rest)
            } else {
                std::mem::take(&mut records)
            };
            let request = ExportLogsServiceRequest {
                resource_logs: vec![ResourceLogs {
                    resource: Some(self.resource.clone()),
                    scope_logs: vec![ScopeLogs {
                        scope: Some(InstrumentationScope {
                            name: TELEMETRY_TARGET.to_owned(),
                            ..Default::default()
                        }),
                        log_records: chunk,
                        schema_url: String::new(),
                    }],
                    schema_url: String::new(),
                }],
            };
            let bytes = request.encode_to_vec();
            if bytes.len() > MAX_DONATION_BYTES {
                tracing::debug!(len = bytes.len(), "dropping oversized log donation payload");
                continue;
            }
            let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
            if self.tx.try_send(PumpMsg::Payload(payload)).is_err() {
                tracing::debug!("log donation queue full; dropping log batch");
            }
        }
    }
}

/// Activation handle swapped into an inert [`DonatingLogLayer`]. Wraps
/// the pump sender plus the resource (`service.name`) the layer needs to
/// encode batches.
pub struct LogDonationSender {
    exporter: PumpLogExporter,
}

impl LogDonationSender {
    fn export(&self, records: Vec<LogRecord>) {
        self.exporter.export(records);
    }
}

#[derive(Default)]
struct LogBatch {
    records: Vec<LogRecord>,
    oldest: Option<Instant>,
}

struct LogLayerShared {
    sender: ArcSwapOption<LogDonationSender>,
    batch: parking_lot::Mutex<LogBatch>,
}

impl LogLayerShared {
    /// Buffer a record; return any records due for flush (count/age).
    fn push(&self, record: LogRecord) -> Vec<LogRecord> {
        let mut batch = self.batch.lock();
        if batch.records.is_empty() {
            batch.oldest = Some(Instant::now());
        }
        batch.records.push(record);
        let due = batch.records.len() >= LOG_BATCH_FLUSH_RECORDS
            || batch
                .oldest
                .is_some_and(|t| t.elapsed() >= LOG_BATCH_MAX_AGE);
        if due {
            batch.oldest = None;
            std::mem::take(&mut batch.records)
        } else {
            Vec::new()
        }
    }

    /// Force the buffered batch onto the pump (teardown analogue of
    /// `fastrace::flush()`); no-op while inert or empty.
    fn flush(&self) {
        let records = {
            let mut batch = self.batch.lock();
            batch.oldest = None;
            std::mem::take(&mut batch.records)
        };
        if records.is_empty() {
            return;
        }
        if let Some(sender) = self.sender.load_full() {
            sender.export(records);
        }
    }
}

/// Process-global handle to the active layer's shared state so
/// [`flush_log_layer`] can drive a teardown flush without a reference.
static ACTIVE_LOG_LAYER: LazyLock<ArcSwapOption<LogLayerShared>> =
    LazyLock::new(ArcSwapOption::empty);

/// A composable [`tracing_subscriber::Layer`] that converts selected
/// events into OTLP log records and batches them onto the pump.
/// Installed inert; activated by [`Self::activate`].
#[derive(Clone)]
pub struct DonatingLogLayer {
    shared: Arc<LogLayerShared>,
}

impl DonatingLogLayer {
    /// Install inert (no sender): selected events are dropped until
    /// [`Self::activate`] swaps a sender in. Registers itself as the
    /// process-global flush target.
    pub fn new_inert() -> Self {
        let shared = Arc::new(LogLayerShared {
            sender: ArcSwapOption::empty(),
            batch: parking_lot::Mutex::new(LogBatch::default()),
        });
        ACTIVE_LOG_LAYER.store(Some(shared.clone()));
        Self { shared }
    }

    /// Swap in the donation sender, activating donation.
    pub fn activate(&self, sender: LogDonationSender) {
        self.shared.sender.store(Some(Arc::new(sender)));
    }
}

impl<S: tracing::Subscriber> Layer<S> for DonatingLogLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let Some(sender) = self.shared.sender.load_full() else {
            return;
        };
        let meta = event.metadata();
        if meta.target() != TELEMETRY_TARGET || !at_least_info(meta.level()) {
            return;
        }
        let mut visitor = AllowlistVisitor::default();
        event.record(&mut visitor);
        let record = build_log_record(meta.level(), visitor);
        let due = self.shared.push(record);
        if !due.is_empty() {
            sender.export(due);
        }
    }
}

/// Flush the active [`DonatingLogLayer`]'s in-memory batch onto the
/// pump. Called from `ToolServer` teardown before the pump drain so a
/// crash-y shutdown does not abandon a partial batch.
pub fn flush_log_layer() {
    if let Some(shared) = ACTIVE_LOG_LAYER.load_full() {
        shared.flush();
    }
}

/// Shutdown fence: drains queued log donations before the connection
/// closes. Call after [`flush_log_layer`].
pub struct LogDonationPump {
    tx: mpsc::Sender<PumpMsg>,
}

impl LogDonationPump {
    /// Resolves once every payload queued before this call has had a
    /// send attempt.
    pub async fn drain(&self) {
        drain_via(&self.tx).await;
    }
}

impl ToolServer {
    /// Post-connect entry point: spawn the log donation pump (wiring
    /// [`ToolServer::donate_logs`]) and return a sender to swap into the
    /// already-installed inert [`DonatingLogLayer`] plus a drain handle.
    /// Does **not** return a `Layer` — a layer cannot be added to an
    /// already-set global subscriber. `service_name` must be
    /// server-allowlisted.
    pub fn log_donation_layer(
        &self,
        service_name: impl Into<String>,
    ) -> (LogDonationSender, LogDonationPump) {
        let (tx, rx) = mpsc::channel::<PumpMsg>(PENDING_FLUSHES);
        let server = self.downgrade();
        tokio::spawn(run_pump(rx, move |payload: String| {
            let server = server.clone();
            async move {
                let Some(server) = server.upgrade() else {
                    return (false, payload);
                };
                let ok = server.donate_logs(&payload).await.is_ok();
                (ok, payload)
            }
        }));
        self.set_log_donation_pump(tx.clone());

        let sender = LogDonationSender {
            exporter: PumpLogExporter {
                tx: tx.clone(),
                resource: make_resource(service_name.into()),
            },
        };
        (sender, LogDonationPump { tx })
    }
}

#[cfg(test)]
mod tests {
    use fastrace::collector::{SpanId, TraceId};
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    fn test_sender(tx: mpsc::Sender<PumpMsg>) -> LogDonationSender {
        LogDonationSender {
            exporter: PumpLogExporter {
                tx,
                resource: make_resource("test-service".to_owned()),
            },
        }
    }

    fn decode(payload: String) -> ExportLogsServiceRequest {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("payload must be base64");
        ExportLogsServiceRequest::decode(bytes.as_slice()).expect("payload must be OTLP")
    }

    #[test]
    fn severity_maps_levels_to_otlp_numbers() {
        assert_eq!(severity(&Level::ERROR), ("ERROR", 17));
        assert_eq!(severity(&Level::WARN), ("WARN", 13));
        assert_eq!(severity(&Level::INFO), ("INFO", 9));
        assert_eq!(severity(&Level::DEBUG), ("DEBUG", 5));
        assert_eq!(severity(&Level::TRACE), ("TRACE", 1));
    }

    #[test]
    fn donation_filter_selects_info_and_above() {
        assert!(at_least_info(&Level::ERROR));
        assert!(at_least_info(&Level::WARN));
        assert!(at_least_info(&Level::INFO));
        assert!(!at_least_info(&Level::DEBUG));
        assert!(!at_least_info(&Level::TRACE));
    }

    #[test]
    fn encode_ids_is_big_endian_16_and_8_bytes() {
        let ctx = SpanContext::new(
            TraceId(0x0af7651916cd43dd8448eb211c80319c),
            SpanId(0xb7ad6b7169203331),
        );
        let (trace_id, span_id) = encode_ids(&ctx);
        assert_eq!(trace_id.len(), 16);
        assert_eq!(span_id.len(), 8);
        assert_eq!(
            format!("{:032x}", u128::from_be_bytes(trace_id.try_into().unwrap())),
            "0af7651916cd43dd8448eb211c80319c"
        );
        assert_eq!(
            format!("{:016x}", u64::from_be_bytes(span_id.try_into().unwrap())),
            "b7ad6b7169203331"
        );
    }

    #[test]
    fn current_ids_empty_without_local_parent() {
        let (trace_id, span_id) = current_ids();
        assert!(trace_id.is_empty());
        assert!(span_id.is_empty());
    }

    #[test]
    fn layer_converts_event_and_redacts_free_form_fields() {
        let layer = DonatingLogLayer::new_inert();
        let (tx, mut rx) = mpsc::channel::<PumpMsg>(8);
        layer.activate(test_sender(tx));
        let flusher = layer.clone();

        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(
                target: "workspace::telemetry",
                session_id = "s1",
                turn_number = 3u64,
                phase = "tool_state",
                error_category = "archive_failed",
                error = "secret git stderr with /home/user/path",
                "archive build failed (queued path)"
            );
            // Off-target event must never be forwarded.
            tracing::warn!(session_id = "s2", "unrelated chatter");
            // DEBUG on-target is below the threshold.
            tracing::debug!(target: "workspace::telemetry", session_id = "s3", "verbose");
        });
        flusher.shared.flush();

        let PumpMsg::Payload(payload) = rx.try_recv().expect("one batch must be queued") else {
            panic!("expected a payload");
        };
        let request = decode(payload);
        let scope_logs = &request.resource_logs[0].scope_logs[0];
        assert_eq!(
            scope_logs.scope.as_ref().unwrap().name,
            "workspace::telemetry"
        );
        assert_eq!(
            scope_logs.log_records.len(),
            1,
            "only the WARN on-target row"
        );

        let record = &scope_logs.log_records[0];
        assert_eq!(record.severity_text, "WARN");
        assert_eq!(record.severity_number, 13);
        assert_eq!(
            record.body.as_ref().unwrap().value,
            Some(any_value::Value::StringValue(
                "archive build failed (queued path)".to_owned()
            ))
        );
        let keys: Vec<&str> = record.attributes.iter().map(|kv| kv.key.as_str()).collect();
        assert!(keys.contains(&"session_id"));
        assert!(keys.contains(&"turn_number"));
        assert!(keys.contains(&"phase"));
        assert!(keys.contains(&"error_category"));
        assert!(
            !keys.contains(&"error"),
            "free-form `error` must be dropped, got {keys:?}"
        );

        // Resource carries the donor service.name.
        let service_name = request.resource_logs[0]
            .resource
            .as_ref()
            .unwrap()
            .attributes
            .iter()
            .find(|kv| kv.key == "service.name")
            .and_then(|kv| kv.value.as_ref())
            .and_then(|v| v.value.clone());
        assert_eq!(
            service_name,
            Some(any_value::Value::StringValue("test-service".to_owned()))
        );

        assert!(rx.try_recv().is_err(), "no further payloads");
    }

    #[test]
    fn inert_layer_drops_selected_events() {
        let layer = DonatingLogLayer::new_inert();
        let flusher = layer.clone();
        // No sender activated.
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(target: "workspace::telemetry", session_id = "s1", "dropped");
        });
        flusher.shared.flush();
        // Nothing to assert beyond not panicking: with no sender the
        // batch never fills and flush is a no-op.
    }

    #[test]
    fn exporter_chunks_at_max_records_per_donation() {
        let (tx, mut rx) = mpsc::channel::<PumpMsg>(8);
        let exporter = PumpLogExporter {
            tx,
            resource: make_resource("test-service".to_owned()),
        };
        let records = vec![LogRecord::default(); MAX_LOG_RECORDS_PER_DONATION + 1];
        exporter.export(records);

        let mut total = 0;
        let mut payloads = 0;
        while let Ok(PumpMsg::Payload(p)) = rx.try_recv() {
            payloads += 1;
            total += decode(p).resource_logs[0].scope_logs[0].log_records.len();
        }
        assert_eq!(payloads, 2, "one full chunk + remainder");
        assert_eq!(total, MAX_LOG_RECORDS_PER_DONATION + 1);
    }
}
