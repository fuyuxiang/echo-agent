use opentelemetry::global;
use opentelemetry::trace::{SpanContext, TraceContextExt, TracerProvider as _};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::{
    InMemorySpanExporter, InMemorySpanExporterBuilder, Sampler, SdkTracerProvider,
    SimpleSpanProcessor,
};
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::prelude::*;

pub struct OtelTestEnv {
    _guard: tracing::subscriber::DefaultGuard,
    provider: SdkTracerProvider,
    exporter: InMemorySpanExporter,
}

impl OtelTestEnv {
    pub fn install() -> Self {
        global::set_text_map_propagator(TraceContextPropagator::new());
        let exporter = InMemorySpanExporterBuilder::new().build();
        // Twin of `echo-agent-tracing-test`: AlwaysOn so children of an unsampled
        // OTel parent still reach the in-memory exporter.
        let provider = SdkTracerProvider::builder()
            .with_sampler(Sampler::AlwaysOn)
            .with_span_processor(SimpleSpanProcessor::new(exporter.clone()))
            .build();
        let tracer = provider.tracer("echo-agent-tracing-test");
        let otel_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_context_activation(false)
            .with_filter(tracing_subscriber::filter::LevelFilter::INFO);
        let guard = tracing_subscriber::registry()
            .with(otel_layer)
            .set_default();
        Self {
            _guard: guard,
            provider,
            exporter,
        }
    }

    pub fn finished_spans(&self) -> Vec<opentelemetry_sdk::trace::SpanData> {
        let _ = self.provider.force_flush();
        self.exporter.get_finished_spans().unwrap_or_default()
    }
}

pub fn parse_traceparent(value: &str) -> (&str, &str, &str) {
    let mut parts = value.split('-');
    let version = parts.next().expect("version");
    let trace_id = parts.next().expect("trace_id");
    let span_id = parts.next().expect("span_id");
    (version, trace_id, span_id)
}

pub fn otel_span_id_hex(span: &Span) -> String {
    let cx = span.context();
    let span_ref = cx.span();
    let sc: &SpanContext = span_ref.span_context();
    format!("{:016x}", u64::from_be_bytes(sc.span_id().to_bytes()))
}

pub fn otel_trace_id_hex(span: &Span) -> String {
    let cx = span.context();
    let span_ref = cx.span();
    let sc: &SpanContext = span_ref.span_context();
    format!("{:032x}", u128::from_be_bytes(sc.trace_id().to_bytes()))
}
