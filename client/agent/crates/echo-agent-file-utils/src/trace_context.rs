use opentelemetry::global;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// Extract the current span's W3C `traceparent` string for propagation
/// across channel/task boundaries where span context is lost.
pub fn current_traceparent() -> Option<String> {
    let current_span = tracing::Span::current();
    if current_span.is_none() {
        return None;
    }

    let cx = current_span.context();
    let mut carrier = std::collections::HashMap::new();
    global::get_text_map_propagator(|p| {
        p.inject_context(&cx, &mut carrier);
    });

    carrier.remove("traceparent")
}

pub fn inject_trace_context_into_request(
    mut builder: reqwest::RequestBuilder,
) -> reqwest::RequestBuilder {
    let mut headers = HeaderMap::new();
    inject_trace_context(&mut headers);

    // Insert new headers into the request builder
    for (name, value) in headers.iter() {
        builder = builder.header(name, value);
    }

    builder
}

/// Return trace-context headers (traceparent, tracestate) for the current
/// span.  Used by callers that hold a `reqwest_middleware::RequestBuilder`
/// (which is a different type from `reqwest::RequestBuilder`).
pub(crate) fn trace_context_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    inject_trace_context(&mut headers);
    headers
}

pub(crate) fn inject_trace_context(headers: &mut HeaderMap) {
    // Prefer the context from the current tracing span (set by OpenTelemetryLayer).
    // Fall back to opentelemetry::Context::current() (thread-local) for code paths
    // that run outside a tracing span but on a thread that has an attached OTel context
    // (e.g. tasks created via spawn_local that inherit the thread-local context).
    let current_span = tracing::Span::current();
    let cx = if current_span.is_none() {
        opentelemetry::Context::current()
    } else {
        current_span.context()
    };

    global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&cx, &mut HeaderMapInjector(headers));
    });
}

struct HeaderMapInjector<'a>(&'a mut HeaderMap);

impl opentelemetry::propagation::Injector for HeaderMapInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        match (HeaderName::try_from(key), HeaderValue::try_from(&value)) {
            (Ok(name), Ok(val)) => {
                self.0.insert(name, val);
            }
            (Err(e), _) => {
                tracing::debug!("Invalid header name '{}': {}", key, e);
            }
            (_, Err(e)) => {
                tracing::debug!("Invalid header value for '{}': {}", key, e);
            }
        }
    }
}

/// Create a tracing span parented to `_meta.traceparent`.
/// Used as a callback for `with_on_meta` in ACP session/server builders.
pub fn span_from_meta_traceparent(
    meta: &serde_json::Map<String, serde_json::Value>,
) -> tracing::Span {
    let span = tracing::info_span!("acp_dispatch");
    if let Some(ctx) = meta
        .get("traceparent")
        .and_then(|v| v.as_str())
        .and_then(extract_context)
    {
        let _ = span.set_parent(ctx);
    }
    span
}

/// Link the current span to a W3C `traceparent` carried inside a JSON `_meta`
/// (or top-level) object. Call this at the top of a `#[tracing::instrument]`
/// function so the span created by the macro becomes a child of the client's
/// distributed trace.
pub fn link_current_span_to_meta(meta: &serde_json::Value) {
    if let Some(ctx) = meta
        .get("traceparent")
        .and_then(|v| v.as_str())
        .and_then(extract_context)
    {
        let _ = tracing::Span::current().set_parent(ctx);
    }
}

fn extract_context(traceparent: &str) -> Option<opentelemetry::Context> {
    use opentelemetry::trace::TraceContextExt;

    let mut carrier = std::collections::HashMap::new();
    carrier.insert("traceparent".to_string(), traceparent.to_string());

    let ctx = opentelemetry::global::get_text_map_propagator(|p| p.extract(&carrier));
    ctx.span().span_context().is_valid().then_some(ctx)
}

#[allow(clippy::disallowed_methods)] // test clients hit localhost mocks
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_trace_context_no_active_span() {
        // When there's no active span, no headers should be added
        let mut headers = HeaderMap::new();
        inject_trace_context(&mut headers);

        // Without an active OpenTelemetry span, no traceparent header is added
        // (the propagator only injects if there's a valid span context)
        assert!(headers.get("traceparent").is_none());
    }

    #[test]
    fn test_header_map_injector_valid_header() {
        let mut headers = HeaderMap::new();
        {
            let mut injector = HeaderMapInjector(&mut headers);
            opentelemetry::propagation::Injector::set(
                &mut injector,
                "traceparent",
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
            );
        }

        assert_eq!(
            headers.get("traceparent").map(|v| v.to_str().unwrap()),
            Some("00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01")
        );
    }

    #[test]
    fn test_header_map_injector_invalid_header_name() {
        let mut headers = HeaderMap::new();
        {
            let mut injector = HeaderMapInjector(&mut headers);
            // Invalid header name (contains space) should be silently ignored
            opentelemetry::propagation::Injector::set(
                &mut injector,
                "invalid header",
                "value".to_string(),
            );
        }

        assert!(headers.is_empty());
    }

    #[test]
    fn test_header_map_injector_invalid_header_value() {
        let mut headers = HeaderMap::new();
        {
            let mut injector = HeaderMapInjector(&mut headers);
            // Invalid header value (contains non-visible ASCII) should be silently ignored
            opentelemetry::propagation::Injector::set(
                &mut injector,
                "traceparent",
                "invalid\x00value".to_string(),
            );
        }

        assert!(headers.is_empty());
    }

    #[test]
    fn test_extract_context_rejects_invalid_traceparent() {
        assert!(extract_context("not-a-valid-traceparent").is_none());
        assert!(extract_context("").is_none());
    }

    /// E2E: _meta.traceparent -> link_current_span_to_meta -> current span
    /// -> inject_trace_context_into_request -> outbound HTTP header carries same traceId.
    #[test]
    fn test_link_meta_then_inject_propagates_trace_id() {
        use opentelemetry::trace::TracerProvider as _;
        use opentelemetry_sdk::propagation::TraceContextPropagator;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use tracing_subscriber::layer::SubscriberExt;

        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("test");
        let otel_layer = tracing_opentelemetry::layer()
            .with_tracer(tracer)
            .with_context_activation(false);
        let subscriber = tracing_subscriber::Registry::default().with(otel_layer);
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);

        let browser_trace_id = "0af7651916cd43dd8448eb211c80319c";
        let meta = serde_json::json!({
            "traceparent": format!("00-{browser_trace_id}-b7ad6b7169203331-01"),
        });

        let span = tracing::info_span!("test_span");
        let _entered = span.enter();
        link_current_span_to_meta(&meta);

        let client = reqwest::Client::new();
        let builder = client.get("https://cli-chat-proxy.example.com/v1/chat/completions");
        let builder = inject_trace_context_into_request(builder);
        let request = builder.build().expect("Failed to build request");

        let traceparent = request
            .headers()
            .get("traceparent")
            .expect("traceparent header missing")
            .to_str()
            .unwrap();

        assert!(
            traceparent.starts_with(&format!("00-{browser_trace_id}-")),
            "outbound traceId should match browser's. got: {traceparent}"
        );
        assert!(
            traceparent.ends_with("-01"),
            "sampled flag should be set. got: {traceparent}"
        );
    }

    #[test]
    fn test_inject_trace_context_into_request_preserves_existing_headers() {
        use opentelemetry::trace::{
            SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
        };
        use opentelemetry_sdk::propagation::TraceContextPropagator;

        // Initialize the global text map propagator for this test
        // This is necessary because by default the global propagator is a no-op
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        // Create a valid span context with known trace_id and span_id
        let trace_id = TraceId::from_hex("0af7651916cd43dd8448eb211c80319c").unwrap();
        let span_id = SpanId::from_hex("b7ad6b7169203331").unwrap();
        let span_context = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::SAMPLED,
            true, // is_remote
            TraceState::default(),
        );

        // Create a context with this span context attached
        let cx = opentelemetry::Context::current().with_remote_span_context(span_context);
        let _guard = cx.attach();

        // Create a client and request builder with existing headers
        let client = reqwest::Client::new();
        let builder = client
            .get("https://example.com")
            .header("x-custom-header", "custom-value")
            .header("authorization", "Bearer token123");

        // Inject trace context into the request
        let builder = inject_trace_context_into_request(builder);
        let request = builder.build().expect("Failed to build request");
        let headers = request.headers();

        // Verify existing headers are preserved
        assert_eq!(
            headers.get("x-custom-header").map(|v| v.to_str().unwrap()),
            Some("custom-value"),
            "Custom header should be preserved after injecting trace context"
        );
        assert_eq!(
            headers.get("authorization").map(|v| v.to_str().unwrap()),
            Some("Bearer token123"),
            "Authorization header should be preserved after injecting trace context"
        );

        // Verify the traceparent header was injected with correct trace context
        let traceparent = headers
            .get("traceparent")
            .expect("traceparent header should be present with active span")
            .to_str()
            .unwrap();

        // traceparent format: {version}-{trace-id}-{parent-id}-{trace-flags}
        // e.g., "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
        assert!(
            traceparent.starts_with("00-0af7651916cd43dd8448eb211c80319c-"),
            "traceparent should contain the correct trace_id, got: {}",
            traceparent
        );
        assert!(
            traceparent.contains("b7ad6b7169203331"),
            "traceparent should contain the correct span_id, got: {}",
            traceparent
        );
        assert!(
            traceparent.ends_with("-01"),
            "traceparent should have sampled flag set, got: {}",
            traceparent
        );
    }
}
