use fastrace::prelude::*;
use fastrace_opentelemetry::OpenTelemetryReporter;
use opentelemetry::InstrumentationScope;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_otlp::{ExporterBuildError, SpanExporter};
use opentelemetry_sdk::Resource;
use std::borrow::Cow;
use std::iter;

// Fastrace initialization
pub fn init_fastrace(
    endpoint: String,
    name: String,
    resource_attributes: impl IntoIterator<Item = (String, String)>,
) -> Result<(), ExporterBuildError> {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .with_protocol(opentelemetry_otlp::Protocol::Grpc)
        .with_timeout(opentelemetry_otlp::OTEL_EXPORTER_OTLP_TIMEOUT_DEFAULT)
        .build()?;
    let attributes = resource_attributes
        .into_iter()
        .chain(iter::once(("service.name".into(), name.clone())))
        .map(|(k, v)| KeyValue::new(k, v));
    let reporter = OpenTelemetryReporter::new(
        exporter,
        Cow::Owned(Resource::builder().with_attributes(attributes).build()),
        InstrumentationScope::builder(name)
            .with_version(env!("CARGO_PKG_VERSION"))
            .build(),
    );
    fastrace::set_reporter(reporter, fastrace::collector::Config::default());
    Ok(())
}

pub fn current_trace_id() -> Option<String> {
    SpanContext::current_local_parent().map(|current| current.encode_w3c_traceparent())
}

pub fn local_or_random_span_ctx() -> SpanContext {
    SpanContext::current_local_parent().unwrap_or_else(SpanContext::random)
}

pub fn enter_span_with_traceparent(name: impl Into<Cow<'static, str>>, traceparent: &str) -> Span {
    if let Some(span_ctx) = SpanContext::decode_w3c_traceparent(traceparent) {
        Span::root(name, span_ctx)
    } else {
        Span::enter_with_local_parent(name)
    }
}

// Tonic channel (TODO: Move into grpc_client when deprecated tracing)
#[allow(dead_code)]
pub type FastraceChannel = fastrace_tonic::FastraceClientService<tonic::transport::Channel>;

pub fn fastrace_channel(
    channel: tonic::transport::Channel,
) -> fastrace_tonic::FastraceClientService<tonic::transport::Channel> {
    tower::ServiceBuilder::new()
        .layer(fastrace_tonic::FastraceClientLayer)
        .service(channel)
}

// Request middleware (TODO: Move into http_client when deprecated tracing)
#[derive(Clone)]
#[allow(dead_code)]
pub struct TraceparentMiddleware;

#[async_trait::async_trait]
impl reqwest_middleware::Middleware for TraceparentMiddleware {
    async fn handle(
        &self,
        mut req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        req.headers_mut()
            .extend(fastrace_reqwest::traceparent_headers());
        next.run(req, extensions).await
    }
}
