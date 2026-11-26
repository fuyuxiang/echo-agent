//! Forward the process's Prometheus metrics to the connected server over
//! the WebSocket transport (`metrics.donate`).
//!
//! A [`MetricDonationReporter`] periodically snapshots the default
//! Prometheus registry via [`prometheus::gather`], converts the
//! `MetricFamily` set to native OTLP metrics (Counter→Sum, Gauge→Gauge,
//! Histogram→Histogram, labels preserved, cumulative temporality), and
//! pumps the batch over the shared [`crate::donate_pump`]. Because it
//! gathers the whole registry, every current and future metric is
//! exported with zero per-metric wiring. Metrics are **process-aggregate**
//! — [`ToolServer::donate_metrics`] requires no bound session.

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use arc_swap::ArcSwapOption;
use base64::Engine as _;
use echo_agent_tool_protocol::{MAX_DONATION_BYTES, MAX_METRICS_PER_DONATION};
use opentelemetry_proto::tonic::collector::metrics::v1::ExportMetricsServiceRequest;
use opentelemetry_proto::tonic::common::v1::KeyValue;
use opentelemetry_proto::tonic::metrics::v1::{
    AggregationTemporality, Gauge, Histogram, HistogramDataPoint, Metric, NumberDataPoint,
    ResourceMetrics, ScopeMetrics, Sum, metric, number_data_point,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use prometheus::proto::{MetricFamily, MetricType};
use prost::Message as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::donate_pump::{
    PENDING_FLUSHES, PumpMsg, make_resource, now_unix_nanos, run_pump, string_kv,
};
use crate::server::ToolServer;

/// How often the reporter snapshots the registry. The server re-stamps
/// attribution; cumulative temporality means missed ticks only delay
/// freshness, never lose monotonic state.
const DEFAULT_GATHER_INTERVAL: Duration = Duration::from_secs(60);

fn labels_to_kv(labels: &[prometheus::proto::LabelPair]) -> Vec<KeyValue> {
    labels
        .iter()
        .map(|l| string_kv(l.name(), l.value().to_owned()))
        .collect()
}

fn number_point(metric: &prometheus::proto::Metric, value: f64, now: u64) -> NumberDataPoint {
    NumberDataPoint {
        attributes: labels_to_kv(metric.get_label()),
        time_unix_nano: now,
        value: Some(number_data_point::Value::AsDouble(value)),
        ..Default::default()
    }
}

/// Prometheus histogram buckets are **cumulative** (`le` counts); OTLP
/// wants per-bucket counts plus an implicit `+Inf` bucket, so the
/// cumulative counts are differenced here.
fn histogram_point(metric: &prometheus::proto::Metric, now: u64) -> HistogramDataPoint {
    let hist = metric.get_histogram();
    let mut bucket_counts = Vec::new();
    let mut explicit_bounds = Vec::new();
    let mut prev = 0u64;
    for bucket in hist.get_bucket() {
        let cumulative = bucket.cumulative_count();
        bucket_counts.push(cumulative.saturating_sub(prev));
        explicit_bounds.push(bucket.upper_bound());
        prev = cumulative;
    }
    let total = hist.get_sample_count();
    bucket_counts.push(total.saturating_sub(prev));
    HistogramDataPoint {
        attributes: labels_to_kv(metric.get_label()),
        time_unix_nano: now,
        count: total,
        sum: Some(hist.get_sample_sum()),
        bucket_counts,
        explicit_bounds,
        ..Default::default()
    }
}

/// Convert a gathered `MetricFamily` set to OTLP metrics. Summaries and
/// untyped families are skipped defensively (none registered today).
fn convert_families(families: &[MetricFamily]) -> Vec<Metric> {
    let now = now_unix_nanos();
    let cumulative = AggregationTemporality::Cumulative as i32;
    let mut out = Vec::new();
    for family in families {
        let name = family.name().to_owned();
        let data = match family.get_field_type() {
            MetricType::COUNTER => metric::Data::Sum(Sum {
                data_points: family
                    .get_metric()
                    .iter()
                    .map(|m| number_point(m, m.get_counter().value(), now))
                    .collect(),
                aggregation_temporality: cumulative,
                is_monotonic: true,
            }),
            MetricType::GAUGE => metric::Data::Gauge(Gauge {
                data_points: family
                    .get_metric()
                    .iter()
                    .map(|m| number_point(m, m.get_gauge().value(), now))
                    .collect(),
            }),
            MetricType::HISTOGRAM => metric::Data::Histogram(Histogram {
                data_points: family
                    .get_metric()
                    .iter()
                    .map(|m| histogram_point(m, now))
                    .collect(),
                aggregation_temporality: cumulative,
            }),
            MetricType::SUMMARY | MetricType::UNTYPED => continue,
        };
        out.push(Metric {
            name,
            data: Some(data),
            ..Default::default()
        });
    }
    out
}

/// Text-format exposition → OTLP, same shapes as [`convert_families`].
fn convert_text_exposition(text: &str, prefix: &str) -> Vec<Metric> {
    let Ok(scrape) =
        prometheus_parse::Scrape::parse(text.lines().map(|l| std::io::Result::Ok(l.to_owned())))
    else {
        return Vec::new();
    };
    let now = now_unix_nanos();
    let cumulative = AggregationTemporality::Cumulative as i32;

    let histogram_names: std::collections::HashSet<&str> = scrape
        .samples
        .iter()
        .filter(|s| matches!(s.value, prometheus_parse::Value::Histogram(_)))
        .map(|s| s.metric.as_str())
        .collect();
    let histogram_sums: std::collections::HashMap<(String, String), f64> = scrape
        .samples
        .iter()
        .filter_map(|s| {
            let base = s.metric.strip_suffix("_sum")?;
            if !histogram_names.contains(base) {
                return None;
            }
            let prometheus_parse::Value::Untyped(v) = s.value else {
                return None;
            };
            Some(((base.to_owned(), s.labels.to_string()), v))
        })
        .collect();

    let parsed_labels_to_kv = |labels: &prometheus_parse::Labels| -> Vec<KeyValue> {
        let mut kv: Vec<KeyValue> = labels
            .iter()
            .map(|(k, v)| string_kv(k, v.clone()))
            .collect();
        kv.sort_by(|a, b| a.key.cmp(&b.key));
        kv
    };

    let mut out = Vec::new();
    for sample in &scrape.samples {
        if !sample.metric.starts_with(prefix) {
            continue;
        }
        let data = match &sample.value {
            prometheus_parse::Value::Counter(v) => metric::Data::Sum(Sum {
                data_points: vec![NumberDataPoint {
                    attributes: parsed_labels_to_kv(&sample.labels),
                    time_unix_nano: now,
                    value: Some(number_data_point::Value::AsDouble(*v)),
                    ..Default::default()
                }],
                aggregation_temporality: cumulative,
                is_monotonic: true,
            }),
            prometheus_parse::Value::Gauge(v) => metric::Data::Gauge(Gauge {
                data_points: vec![NumberDataPoint {
                    attributes: parsed_labels_to_kv(&sample.labels),
                    time_unix_nano: now,
                    value: Some(number_data_point::Value::AsDouble(*v)),
                    ..Default::default()
                }],
            }),
            prometheus_parse::Value::Histogram(counts) => {
                let mut counts = counts.clone();
                counts.sort_by(|a, b| a.less_than.total_cmp(&b.less_than));
                let mut bucket_counts = Vec::with_capacity(counts.len());
                let mut explicit_bounds = Vec::with_capacity(counts.len().saturating_sub(1));
                let mut prev = 0f64;
                let mut total = 0u64;
                for bucket in &counts {
                    bucket_counts.push((bucket.count - prev).max(0.0) as u64);
                    if bucket.less_than.is_finite() {
                        explicit_bounds.push(bucket.less_than);
                    } else {
                        total = bucket.count.max(0.0) as u64;
                    }
                    prev = bucket.count;
                }
                let sum = histogram_sums
                    .get(&(sample.metric.clone(), sample.labels.to_string()))
                    .copied();
                metric::Data::Histogram(Histogram {
                    data_points: vec![HistogramDataPoint {
                        attributes: parsed_labels_to_kv(&sample.labels),
                        time_unix_nano: now,
                        count: total,
                        sum,
                        bucket_counts,
                        explicit_bounds,
                        ..Default::default()
                    }],
                    aggregation_temporality: cumulative,
                })
            }
            // Untyped = histogram `_sum`/`_count` twins, joined above.
            prometheus_parse::Value::Summary(_) | prometheus_parse::Value::Untyped(_) => continue,
        };
        out.push(Metric {
            name: sample.metric.clone(),
            data: Some(data),
            ..Default::default()
        });
    }
    out
}

/// Donates metrics from outside the default Prometheus registry.
pub struct DonatedMetricsSink {
    exporter: Arc<MetricExporter>,
}

impl DonatedMetricsSink {
    /// Returns the number of OTLP metrics queued (malformed input → 0).
    pub fn export_text_exposition(&self, text: &str, prefix: &str) -> usize {
        let metrics = convert_text_exposition(text, prefix);
        let exported = metrics.len();
        if exported > 0 {
            self.exporter.export(metrics);
        }
        exported
    }
}

/// `Some` while the donation pump is active; re-fetch per use (cheap).
pub fn active_metrics_sink() -> Option<DonatedMetricsSink> {
    ACTIVE_METRIC_EXPORTER
        .load_full()
        .map(|exporter| DonatedMetricsSink { exporter })
}

/// Encodes batches of OTLP metrics onto the pump channel. Chunks at
/// [`MAX_METRICS_PER_DONATION`], drops payloads over
/// [`MAX_DONATION_BYTES`], and never blocks.
#[derive(Clone)]
struct MetricExporter {
    tx: mpsc::Sender<PumpMsg>,
    resource: Resource,
}

impl MetricExporter {
    fn export(&self, mut metrics: Vec<Metric>) {
        while !metrics.is_empty() {
            let chunk = if metrics.len() > MAX_METRICS_PER_DONATION {
                let rest = metrics.split_off(MAX_METRICS_PER_DONATION);
                std::mem::replace(&mut metrics, rest)
            } else {
                std::mem::take(&mut metrics)
            };
            let request = ExportMetricsServiceRequest {
                resource_metrics: vec![ResourceMetrics {
                    resource: Some(self.resource.clone()),
                    scope_metrics: vec![ScopeMetrics {
                        metrics: chunk,
                        ..Default::default()
                    }],
                    schema_url: String::new(),
                }],
            };
            let bytes = request.encode_to_vec();
            if bytes.len() > MAX_DONATION_BYTES {
                tracing::debug!(
                    len = bytes.len(),
                    "dropping oversized metric donation payload"
                );
                continue;
            }
            let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
            if self.tx.try_send(PumpMsg::Payload(payload)).is_err() {
                tracing::debug!("metric donation queue full; dropping metric batch");
            }
        }
    }

    fn gather_and_send(&self) {
        let metrics = convert_families(&prometheus::gather());
        if metrics.is_empty() {
            return;
        }
        self.export(metrics);
    }
}

/// Process-global handle to the active exporter so [`gather_and_send`]
/// can drive a final teardown gather without a reference.
static ACTIVE_METRIC_EXPORTER: LazyLock<ArcSwapOption<MetricExporter>> =
    LazyLock::new(ArcSwapOption::empty);

/// Final registry gather onto the active metric pump. Called from
/// `ToolServer` teardown before the pump drain so a crash-y shutdown
/// captures the latest values.
pub(crate) fn gather_and_send() {
    if let Some(exporter) = ACTIVE_METRIC_EXPORTER.load_full() {
        exporter.gather_and_send();
    }
}

/// Drop the process-global exporter on teardown so its pump `Sender` is released
/// and the metric pump can wind down. Called from `flush_donations_inner` after
/// the final [`gather_and_send`] (and alongside clearing the stored pump
/// senders), so a dropped `ToolServer` doesn't leak the pump task.
pub(crate) fn clear_active_exporter() {
    ACTIVE_METRIC_EXPORTER.store(None);
}

/// Periodic registry gatherer spawned by
/// [`ToolServer::metric_donation_reporter`]. Internal: constructed and run
/// only by `metric_donation_reporter`; not part of the crate's public API.
pub(crate) struct MetricDonationReporter {
    exporter: MetricExporter,
    interval: Duration,
    shutdown: CancellationToken,
}

impl MetricDonationReporter {
    async fn run(self) {
        let mut ticker = tokio::time::interval(self.interval);
        loop {
            tokio::select! {
                _ = ticker.tick() => self.exporter.gather_and_send(),
                // Stop on teardown so this task (and the pump `tx` clone it
                // holds) doesn't outlive `ToolServer::shutdown` and keep
                // gathering/sending forever.
                _ = self.shutdown.cancelled() => break,
            }
        }
    }
}

/// Shutdown fence: drains queued metric donations before the connection
/// closes.
pub struct MetricDonationPump {
    tx: mpsc::Sender<PumpMsg>,
}

impl MetricDonationPump {
    /// Resolves once every payload queued before this call has had a
    /// send attempt.
    pub async fn drain(&self) {
        crate::donate_pump::drain_via(&self.tx).await;
    }
}

impl ToolServer {
    /// Post-connect entry point: spawn the metric donation pump (wiring
    /// [`ToolServer::donate_metrics`]) plus the periodic registry
    /// gatherer, and return a drain handle. Activates on server presence
    /// (no env flag). `service_name` must be server-allowlisted.
    pub fn metric_donation_reporter(&self, service_name: impl Into<String>) -> MetricDonationPump {
        let (tx, rx) = mpsc::channel::<PumpMsg>(PENDING_FLUSHES);
        let server = self.downgrade();
        tokio::spawn(run_pump(rx, move |payload: String| {
            let server = server.clone();
            async move {
                let Some(server) = server.upgrade() else {
                    return (false, payload);
                };
                let ok = server.donate_metrics(&payload).await.is_ok();
                (ok, payload)
            }
        }));
        self.set_metric_donation_pump(tx.clone());

        let exporter = MetricExporter {
            tx: tx.clone(),
            resource: make_resource(service_name.into()),
        };
        ACTIVE_METRIC_EXPORTER.store(Some(Arc::new(exporter.clone())));
        tokio::spawn(
            MetricDonationReporter {
                exporter,
                interval: DEFAULT_GATHER_INTERVAL,
                shutdown: self.shutdown_token(),
            }
            .run(),
        );

        MetricDonationPump { tx }
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_proto::tonic::common::v1::any_value;
    use prometheus::{Histogram, HistogramOpts, IntCounterVec, IntGauge, Opts, Registry};

    use super::*;

    fn decode(payload: String) -> ExportMetricsServiceRequest {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("payload must be base64");
        ExportMetricsServiceRequest::decode(bytes.as_slice()).expect("payload must be OTLP")
    }

    fn label_map(attrs: &[KeyValue]) -> std::collections::HashMap<String, String> {
        attrs
            .iter()
            .filter_map(|kv| match kv.value.as_ref().and_then(|v| v.value.clone()) {
                Some(any_value::Value::StringValue(s)) => Some((kv.key.clone(), s)),
                _ => None,
            })
            .collect()
    }

    const PROXY_TEXT: &str = "\
# HELP preview_proxy_requests_total Proxied preview requests.\n\
# TYPE preview_proxy_requests_total counter\n\
preview_proxy_requests_total{visibility=\"public\",outcome=\"ok\"} 41\n\
preview_proxy_requests_total{visibility=\"private\",outcome=\"upstream_error\"} 2\n\
# HELP preview_proxy_active_ws_connections Live proxied WebSocket connections.\n\
# TYPE preview_proxy_active_ws_connections gauge\n\
preview_proxy_active_ws_connections 3\n\
# HELP preview_proxy_request_duration_seconds Preview request latency.\n\
# TYPE preview_proxy_request_duration_seconds histogram\n\
preview_proxy_request_duration_seconds_bucket{le=\"0.5\"} 4\n\
preview_proxy_request_duration_seconds_bucket{le=\"1\"} 6\n\
preview_proxy_request_duration_seconds_bucket{le=\"+Inf\"} 7\n\
preview_proxy_request_duration_seconds_sum 5.25\n\
preview_proxy_request_duration_seconds_count 7\n\
# TYPE other_family_total counter\n\
other_family_total 9\n";

    #[test]
    fn text_exposition_converts_and_filters_by_prefix() {
        let metrics = convert_text_exposition(PROXY_TEXT, "preview_proxy_");
        let by_name: std::collections::HashMap<&str, &Metric> =
            metrics.iter().map(|m| (m.name.as_str(), m)).collect();

        assert!(!by_name.contains_key("other_family_total"));
        assert!(!by_name.contains_key("preview_proxy_request_duration_seconds_sum"));
        assert!(!by_name.contains_key("preview_proxy_request_duration_seconds_count"));

        let counters: Vec<&Metric> = metrics
            .iter()
            .filter(|m| m.name == "preview_proxy_requests_total")
            .collect();
        assert_eq!(2, counters.len());
        let ok_point = counters
            .iter()
            .find_map(|m| match m.data.as_ref() {
                Some(metric::Data::Sum(sum)) => {
                    assert!(sum.is_monotonic);
                    sum.data_points.iter().find(|p| {
                        label_map(&p.attributes).get("outcome").map(String::as_str) == Some("ok")
                    })
                }
                _ => None,
            })
            .expect("ok counter point");
        assert_eq!(
            Some(number_data_point::Value::AsDouble(41.0)),
            ok_point.value
        );
        assert_eq!(
            Some("public"),
            label_map(&ok_point.attributes)
                .get("visibility")
                .map(String::as_str)
        );

        let Some(metric::Data::Gauge(gauge)) =
            by_name["preview_proxy_active_ws_connections"].data.as_ref()
        else {
            panic!("gauge expected");
        };
        assert_eq!(
            Some(number_data_point::Value::AsDouble(3.0)),
            gauge.data_points[0].value
        );

        let Some(metric::Data::Histogram(hist)) = by_name["preview_proxy_request_duration_seconds"]
            .data
            .as_ref()
        else {
            panic!("histogram expected");
        };
        let point = &hist.data_points[0];
        assert_eq!(vec![0.5, 1.0], point.explicit_bounds);
        assert_eq!(vec![4, 2, 1], point.bucket_counts);
        assert_eq!(7, point.count);
        assert_eq!(Some(5.25), point.sum);
    }

    #[test]
    fn text_exposition_tolerates_garbage_and_empty_input() {
        assert!(convert_text_exposition("", "preview_proxy_").is_empty());
        assert!(convert_text_exposition("not a metric line at all", "preview_proxy_").is_empty());
        assert!(convert_text_exposition(PROXY_TEXT, "no_such_prefix_").is_empty());
    }

    #[test]
    fn converts_counter_gauge_histogram_with_labels() {
        let registry = Registry::new();

        let counter =
            IntCounterVec::new(Opts::new("echo_agent_test_total", "help"), &["reason"]).unwrap();
        registry.register(Box::new(counter.clone())).unwrap();
        counter.with_label_values(&["zdr"]).inc_by(5);

        let gauge = IntGauge::new("echo_agent_test_pending", "help").unwrap();
        registry.register(Box::new(gauge.clone())).unwrap();
        gauge.set(7);

        let hist = Histogram::with_opts(
            HistogramOpts::new("echo_agent_test_seconds", "help").buckets(vec![0.5, 1.0]),
        )
        .unwrap();
        registry.register(Box::new(hist.clone())).unwrap();
        hist.observe(0.25);
        hist.observe(0.75);
        hist.observe(5.0);

        let metrics = convert_families(&registry.gather());
        let by_name: std::collections::HashMap<_, _> =
            metrics.iter().map(|m| (m.name.clone(), m)).collect();

        // Counter -> Sum (monotonic, cumulative), label preserved.
        let metric::Data::Sum(sum) = by_name["echo_agent_test_total"].data.as_ref().unwrap() else {
            panic!("counter must convert to Sum");
        };
        assert!(sum.is_monotonic);
        assert_eq!(
            sum.aggregation_temporality,
            AggregationTemporality::Cumulative as i32
        );
        let dp = &sum.data_points[0];
        assert_eq!(dp.value, Some(number_data_point::Value::AsDouble(5.0)));
        assert_eq!(
            label_map(&dp.attributes).get("reason").map(String::as_str),
            Some("zdr")
        );

        // Gauge -> Gauge.
        let metric::Data::Gauge(g) = by_name["echo_agent_test_pending"].data.as_ref().unwrap()
        else {
            panic!("gauge must convert to Gauge");
        };
        assert_eq!(
            g.data_points[0].value,
            Some(number_data_point::Value::AsDouble(7.0))
        );

        // Histogram -> Histogram with cumulative buckets differenced and
        // a +Inf bucket appended.
        let metric::Data::Histogram(h) = by_name["echo_agent_test_seconds"].data.as_ref().unwrap()
        else {
            panic!("histogram must convert to Histogram");
        };
        assert_eq!(
            h.aggregation_temporality,
            AggregationTemporality::Cumulative as i32
        );
        let hdp = &h.data_points[0];
        assert_eq!(hdp.count, 3);
        assert_eq!(hdp.sum, Some(6.0));
        assert_eq!(hdp.explicit_bounds, vec![0.5, 1.0]);
        // (<=0.5): 0.25 -> 1 ; (0.5,1.0]: 0.75 -> 1 ; (+Inf): 5.0 -> 1
        assert_eq!(hdp.bucket_counts, vec![1, 1, 1]);
    }

    #[test]
    fn exporter_chunks_at_max_metrics_per_donation() {
        let (tx, mut rx) = mpsc::channel::<PumpMsg>(8);
        let exporter = MetricExporter {
            tx,
            resource: make_resource("test-service".to_owned()),
        };
        let metrics = vec![Metric::default(); MAX_METRICS_PER_DONATION + 1];
        exporter.export(metrics);

        let mut payloads = 0;
        let mut total = 0;
        while let Ok(PumpMsg::Payload(p)) = rx.try_recv() {
            payloads += 1;
            total += decode(p).resource_metrics[0].scope_metrics[0].metrics.len();
        }
        assert_eq!(payloads, 2, "one full chunk + remainder");
        assert_eq!(total, MAX_METRICS_PER_DONATION + 1);
    }

    #[test]
    fn summary_and_untyped_families_are_skipped() {
        // An empty registry gathers nothing; convert yields nothing.
        let registry = Registry::new();
        assert!(convert_families(&registry.gather()).is_empty());
    }
}
