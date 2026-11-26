use http::{HeaderMap, Request};
use opentelemetry::{global, propagation::Extractor, propagation::Injector};
use std::task::{Context, Poll};
use tonic::transport::Channel;
use tonic::{
    Status,
    metadata::{MetadataKey, MetadataMap, MetadataValue},
};
use tower::{Layer, Service, ServiceBuilder};
use tower_http::classify::{GrpcErrorsAsFailures, SharedClassifier};
use tower_http::trace::{MakeSpan, Trace, TraceLayer};
use tracing::{Span, warn};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub type TracedChannel = Trace<
    InjectTraceContextService<Channel>,
    SharedClassifier<GrpcErrorsAsFailures>,
    MakeClientSpan,
>;

/// Wraps the input channel with a tracing layer. This function can be used to create a traced gRPC
/// client as follows:
///
/// ```rust
/// use tonic::transport::Endpoint;
/// use std::str::FromStr;
/// use echo_agent_tracing::traced_channel;
///
/// let channel = Endpoint::from_str("http://foo").unwrap();
/// //let client = SomeClient::new(traced_channel(channel));
///```
pub fn traced_channel(channel: Channel) -> TracedChannel {
    ServiceBuilder::new()
        .layer(TraceLayer::new_for_grpc().make_span_with(MakeClientSpan))
        .layer(InjectTraceContextLayer)
        .service(channel)
}

/// Implements the [`MakeSpan`] trait, to trace outgoing gRPC requests.
#[derive(Debug, Clone, Copy)]
pub struct MakeClientSpan;

impl<B> MakeSpan<B> for MakeClientSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        // No active dispatcher → the span has no consumer and would only be
        // downgraded to `log` spam. See `crate::dispatcher_active`.
        if !crate::dispatcher_active() {
            return Span::none();
        }
        tracing::info_span!(
            "grpc_request",
            otel.kind = "client",
            method = %request.method(),
            uri = %request.uri(),
            version = ?request.version(),
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InjectTraceContextLayer;

impl<S> Layer<S> for InjectTraceContextLayer {
    type Service = InjectTraceContextService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        InjectTraceContextService { inner }
    }
}

#[derive(Clone, Debug)]
pub struct InjectTraceContextService<S> {
    inner: S,
}

impl<S, B> Service<Request<B>> for InjectTraceContextService<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        crate::http_client::attach_trace_to_http_request(req.headers_mut());
        self.inner.call(req)
    }
}

/// Inject W3C `traceparent` / `tracestate` from the active span into gRPC
/// metadata. Mutates in place so callers never lose the request body.
pub fn attach_trace_to_grpc_request_mut(metadata: &mut MetadataMap) {
    global::get_text_map_propagator(|propagator| {
        let context = Span::current().context();
        propagator.inject_context(&context, &mut MetadataInjector(metadata));
    });
}

/// The current span's W3C `traceparent`, for protocols that carry trace
/// context in a message field instead of transport metadata; `None` when
/// there is no valid span context.
pub fn current_traceparent() -> Option<String> {
    use opentelemetry::trace::TraceContextExt;
    let context = Span::current().context();
    let span_ref = context.span();
    let span_context = span_ref.span_context();
    if !span_context.is_valid() {
        return None;
    }
    Some(format!(
        "00-{}-{}-{:02x}",
        span_context.trace_id(),
        span_context.span_id(),
        span_context.trace_flags() & opentelemetry::TraceFlags::SAMPLED,
    ))
}

/// Trace context propagation: send the trace context by injecting it into the metadata of the given
/// request.
pub fn attach_trace_to_grpc_request<T>(
    mut request: tonic::Request<T>,
) -> Result<tonic::Request<T>, Status> {
    attach_trace_to_grpc_request_mut(request.metadata_mut());
    Ok(request)
}

// Need a custom Injector to inject OTel headers
pub struct MetadataInjector<'a>(&'a mut MetadataMap);

impl Injector for MetadataInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        match MetadataKey::from_bytes(key.as_bytes()) {
            Ok(key) => match MetadataValue::try_from(&value) {
                Ok(value) => {
                    self.0.insert(key, value);
                }

                Err(error) => warn!(value, error = format!("{error:#}"), "parse metadata value"),
            },

            Err(error) => warn!(key, error = format!("{error:#}"), "parse metadata key"),
        }
    }
}

pub struct HeaderExtractor<'a>(pub &'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| {
            let s = v.to_str();
            if let Err(ref error) = s {
                warn!(%error, ?v, "cannot convert header value to ASCII")
            };
            s.ok()
        })
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{OtelTestEnv, otel_span_id_hex, otel_trace_id_hex, parse_traceparent};
    use http_body_util::Empty;
    use std::convert::Infallible;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tower_http::classify::GrpcFailureClass;
    use tracing::Instrument;

    type EmptyBody = Empty<bytes::Bytes>;

    #[derive(Clone)]
    struct CaptureService {
        seen: Arc<Mutex<Option<HeaderMap>>>,
        response_grpc_status: Option<&'static str>,
    }

    impl CaptureService {
        fn new() -> Self {
            Self {
                seen: Arc::new(Mutex::new(None)),
                response_grpc_status: None,
            }
        }

        fn with_grpc_status(status: &'static str) -> Self {
            Self {
                seen: Arc::new(Mutex::new(None)),
                response_grpc_status: Some(status),
            }
        }
    }

    impl<B> Service<Request<B>> for CaptureService {
        type Response = http::Response<EmptyBody>;
        type Error = Infallible;
        type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: Request<B>) -> Self::Future {
            *self.seen.lock().unwrap() = Some(req.headers().clone());
            let mut builder = http::Response::builder().status(200);
            if let Some(status) = self.response_grpc_status {
                builder = builder.header("grpc-status", status);
            }
            std::future::ready(Ok(builder.body(Empty::new()).unwrap()))
        }
    }

    fn post_req() -> Request<EmptyBody> {
        Request::builder()
            .method("POST")
            .uri("http://svc/package.Service/Method")
            .body(Empty::new())
            .unwrap()
    }

    // With no dispatcher active, the client span must not be created —
    // `tracing`'s `log` compat would downgrade it into `grpc_request; ...`
    // log spam in processes that only configure a `log` logger.
    // See `crate::dispatcher_active`.
    #[test]
    fn make_client_span_without_dispatcher_is_none() {
        assert!(MakeClientSpan.make_span(&post_req()).is_none());
    }

    #[test]
    fn make_client_span_with_scoped_dispatcher_is_enabled() {
        let _env = OtelTestEnv::install();
        assert!(!MakeClientSpan.make_span(&post_req()).is_disabled());
    }

    #[tokio::test]
    async fn inject_under_trace_layer_uses_client_span_not_parent() {
        let _env = OtelTestEnv::install();

        let capture = CaptureService::new();
        let seen = Arc::clone(&capture.seen);
        let mut svc = ServiceBuilder::new()
            .layer(TraceLayer::new_for_grpc().make_span_with(MakeClientSpan))
            .layer(InjectTraceContextLayer)
            .service(capture);

        let parent = tracing::info_span!("parent_handler");
        let parent_span_id = otel_span_id_hex(&parent);
        let parent_trace_id = otel_trace_id_hex(&parent);
        assert_ne!(parent_span_id, "0000000000000000");

        async {
            let fut = Service::call(&mut svc, post_req());
            fut.await.unwrap();
        }
        .instrument(parent)
        .await;

        let headers = seen.lock().unwrap().clone().expect("headers");
        let tp = headers
            .get("traceparent")
            .expect("traceparent")
            .to_str()
            .unwrap();
        let (_ver, injected_trace_id, injected_span_id) = parse_traceparent(tp);

        assert_eq!(injected_trace_id, parent_trace_id);
        assert_ne!(injected_span_id, parent_span_id);
    }

    #[tokio::test]
    async fn inject_without_trace_layer_uses_parent_span() {
        let _env = OtelTestEnv::install();

        let capture = CaptureService::new();
        let seen = Arc::clone(&capture.seen);
        let mut svc = ServiceBuilder::new()
            .layer(InjectTraceContextLayer)
            .service(capture);

        let parent = tracing::info_span!("parent_handler");
        let parent_span_id = otel_span_id_hex(&parent);
        let parent_trace_id = otel_trace_id_hex(&parent);

        async {
            let fut = Service::call(&mut svc, post_req());
            fut.await.unwrap();
        }
        .instrument(parent)
        .await;

        let headers = seen.lock().unwrap().clone().expect("headers captured");
        let tp = headers.get("traceparent").unwrap().to_str().unwrap();
        let (_ver, injected_trace_id, injected_span_id) = parse_traceparent(tp);

        assert_eq!(injected_trace_id, parent_trace_id);
        assert_eq!(injected_span_id, parent_span_id);
    }

    #[tokio::test]
    async fn grpc_status_non_ok_invokes_on_failure_classifier() {
        let _env = OtelTestEnv::install();

        let failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let failures_cb = Arc::clone(&failures);

        let capture = CaptureService::with_grpc_status("13");
        let mut svc = ServiceBuilder::new()
            .layer(
                TraceLayer::new_for_grpc()
                    .make_span_with(MakeClientSpan)
                    .on_failure(
                        move |class: GrpcFailureClass,
                              _latency: Duration,
                              _span: &tracing::Span| {
                            failures_cb.lock().unwrap().push(class.to_string());
                        },
                    ),
            )
            .layer(InjectTraceContextLayer)
            .service(capture);

        let fut = Service::call(&mut svc, post_req());
        let _ = fut.await.unwrap();

        let recorded = failures.lock().unwrap().clone();
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        assert!(
            recorded[0].contains("13") || recorded[0].to_lowercase().contains("code"),
            "{}",
            recorded[0]
        );
    }

    #[tokio::test]
    async fn grpc_status_ok_does_not_invoke_on_failure() {
        let _env = OtelTestEnv::install();

        let failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let failures_cb = Arc::clone(&failures);

        let capture = CaptureService::with_grpc_status("0");
        let mut svc = ServiceBuilder::new()
            .layer(
                TraceLayer::new_for_grpc()
                    .make_span_with(MakeClientSpan)
                    .on_failure(
                        move |class: GrpcFailureClass,
                              _latency: Duration,
                              _span: &tracing::Span| {
                            failures_cb.lock().unwrap().push(class.to_string());
                        },
                    ),
            )
            .layer(InjectTraceContextLayer)
            .service(capture);

        let fut = Service::call(&mut svc, post_req());
        let _ = fut.await.unwrap();

        assert!(failures.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn attach_trace_to_grpc_request_sets_traceparent_metadata() {
        let _env = OtelTestEnv::install();
        let span = tracing::info_span!("handler");
        let span_id = otel_span_id_hex(&span);
        let _enter = span.enter();

        let req = attach_trace_to_grpc_request(tonic::Request::new(())).unwrap();
        let tp = req
            .metadata()
            .get("traceparent")
            .expect("traceparent in metadata")
            .to_str()
            .unwrap();
        let (_ver, _tid, injected_span_id) = parse_traceparent(tp);
        assert_eq!(injected_span_id, span_id);
    }

    #[tokio::test]
    async fn make_client_span_records_otel_kind_client() {
        let env = OtelTestEnv::install();
        {
            let req = post_req();
            let mut make = MakeClientSpan;
            let span = make.make_span(&req);
            let _e = span.enter();
        }
        let spans = env.finished_spans();
        let grpc = spans
            .iter()
            .find(|s| s.name == "grpc_request")
            .expect("grpc_request");
        assert_eq!(grpc.span_kind, opentelemetry::trace::SpanKind::Client);
    }
}
