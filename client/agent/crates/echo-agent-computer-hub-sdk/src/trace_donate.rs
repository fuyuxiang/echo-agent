//! Forward selected spans to the connected server over the WebSocket
//! transport (`traces.donate`). The bounded retry buffer + drain barrier
//! live in [`crate::donate_pump`]; overflow drops spans — telemetry,
//! never correctness.

use std::borrow::Cow;

use base64::Engine as _;
use echo_agent_tool_protocol::{MAX_DONATION_BYTES, MAX_SPANS_PER_DONATION};
use fastrace::collector::{Reporter, SpanRecord};
use fastrace_opentelemetry::OpenTelemetryReporter;
use opentelemetry::InstrumentationScope;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::transform::common::tonic::ResourceAttributesWithSchema;
use opentelemetry_proto::transform::trace::tonic::group_spans_by_resource_and_scope;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::OTelSdkResult;
use opentelemetry_sdk::trace::{SpanData, SpanExporter};
use prost::Message as _;
use tokio::sync::mpsc;

use crate::donate_pump::{PENDING_FLUSHES, PumpMsg, drain_via, run_pump};
use crate::server::ToolServer;

/// fastrace [`Reporter`] feeding the donation pump.
pub struct HubDonatingReporter(OpenTelemetryReporter);

impl Reporter for HubDonatingReporter {
    fn report(&mut self, spans: Vec<SpanRecord>) {
        if spans.is_empty() {
            return;
        }
        self.0.report(spans);
    }
}

/// [`SpanExporter`] that encodes OTLP requests onto the pump channel.
/// Runs on fastrace's collector thread; must never block.
#[derive(Debug)]
struct PumpSpanExporter {
    tx: mpsc::Sender<PumpMsg>,
    resource: ResourceAttributesWithSchema,
}

impl SpanExporter for PumpSpanExporter {
    fn export(
        &self,
        batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        let mut remaining = batch;
        while !remaining.is_empty() {
            let chunk = if remaining.len() > MAX_SPANS_PER_DONATION {
                let rest = remaining.split_off(MAX_SPANS_PER_DONATION);
                std::mem::replace(&mut remaining, rest)
            } else {
                std::mem::take(&mut remaining)
            };
            let request = ExportTraceServiceRequest {
                resource_spans: group_spans_by_resource_and_scope(chunk, &self.resource),
            };
            let bytes = request.encode_to_vec();
            if bytes.len() > MAX_DONATION_BYTES {
                tracing::debug!(len = bytes.len(), "dropping oversized donation payload");
                continue;
            }
            let payload = base64::engine::general_purpose::STANDARD.encode(bytes);
            if self.tx.try_send(PumpMsg::Payload(payload)).is_err() {
                tracing::debug!("trace donation queue full; dropping span batch");
            }
        }
        std::future::ready(Ok(()))
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.resource = resource.into();
    }
}

/// Shutdown fence: drains queued donations before the connection closes.
pub struct TraceDonationPump {
    tx: mpsc::Sender<PumpMsg>,
}

impl TraceDonationPump {
    /// Resolves once every payload queued before this call has had a
    /// send attempt. Call after `fastrace::flush()`.
    pub async fn drain(&self) {
        drain_via(&self.tx).await;
    }
}

impl ToolServer {
    /// Spawn the donation pump and return its reporter + drain handle.
    /// `service_name` must be server-allowlisted.
    pub fn trace_donation_reporter(
        &self,
        service_name: impl Into<String>,
    ) -> (HubDonatingReporter, TraceDonationPump) {
        let (tx, rx) = mpsc::channel::<PumpMsg>(PENDING_FLUSHES);
        let server = self.downgrade();
        tokio::spawn(run_pump(rx, move |payload: String| {
            let server = server.clone();
            async move {
                let Some(server) = server.upgrade() else {
                    return (false, payload);
                };
                let ok = server.donate_traces(&payload).await.is_ok();
                (ok, payload)
            }
        }));
        self.set_donation_pump(tx.clone());

        let resource = Resource::builder()
            .with_service_name(service_name.into())
            .build();
        let exporter = PumpSpanExporter {
            tx: tx.clone(),
            resource: (&resource).into(),
        };
        let reporter = OpenTelemetryReporter::new(
            exporter,
            Cow::Owned(resource),
            InstrumentationScope::default(),
        );
        (HubDonatingReporter(reporter), TraceDonationPump { tx })
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use opentelemetry::trace::{SpanContext, SpanKind, Status, TraceFlags, TraceState};

    use super::*;

    #[tokio::test]
    async fn exporter_encodes_standard_otlp_with_resource() {
        let resource = Resource::builder()
            .with_service_name("test-service")
            .build();
        let (tx, mut rx) = mpsc::channel::<PumpMsg>(4);
        let exporter = PumpSpanExporter {
            tx,
            resource: (&resource).into(),
        };

        let span = SpanData {
            span_context: SpanContext::new(
                0x0af7651916cd43dd8448eb211c80319c_u128.into(),
                0xb7ad6b7169203331_u64.into(),
                TraceFlags::SAMPLED,
                false,
                TraceState::default(),
            ),
            parent_span_id: 0_u64.into(),
            parent_span_is_remote: false,
            span_kind: SpanKind::Internal,
            name: "tool_server.tool_call".into(),
            start_time: SystemTime::UNIX_EPOCH,
            end_time: SystemTime::UNIX_EPOCH,
            attributes: vec![opentelemetry::KeyValue::new("tool_id", "bash")],
            dropped_attributes_count: 0,
            events: opentelemetry_sdk::trace::SpanEvents::default(),
            links: opentelemetry_sdk::trace::SpanLinks::default(),
            status: Status::Unset,
            instrumentation_scope: InstrumentationScope::default(),
        };
        exporter
            .export(vec![span])
            .await
            .expect("export must succeed");

        let Some(PumpMsg::Payload(payload)) = rx.try_recv().ok() else {
            panic!("exporter must enqueue one payload");
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("payload must be base64");
        let request =
            ExportTraceServiceRequest::decode(bytes.as_slice()).expect("payload must be OTLP");
        let resource_spans = &request.resource_spans[0];
        let service_name = resource_spans
            .resource
            .as_ref()
            .unwrap()
            .attributes
            .iter()
            .find(|kv| kv.key == "service.name")
            .and_then(|kv| kv.value.as_ref())
            .map(|v| format!("{v:?}"));
        assert!(
            service_name.unwrap_or_default().contains("test-service"),
            "resource must carry the donor service.name"
        );
        let span = &resource_spans.scope_spans[0].spans[0];
        assert_eq!(span.name, "tool_server.tool_call");
        assert_eq!(
            format!(
                "{:032x}",
                u128::from_be_bytes(span.trace_id.as_slice().try_into().unwrap())
            ),
            "0af7651916cd43dd8448eb211c80319c"
        );
    }
}
