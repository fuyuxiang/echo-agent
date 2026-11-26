use async_trait::async_trait;
use opentelemetry::global;
use opentelemetry_http::HeaderInjector;
use reqwest::header::HeaderMap;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware, Middleware};
use tracing::{Instrument, Span, field};
use tracing_opentelemetry::OpenTelemetrySpanExt;

pub fn attach_trace_to_http_request(headers: &mut HeaderMap) {
    global::get_text_map_propagator(|propagator| {
        let context = Span::current().context();
        propagator.inject_context(&context, &mut HeaderInjector(headers));
    });
}

pub type TracedHttpClient = ClientWithMiddleware;

pub fn traced_client(client: reqwest::Client) -> TracedHttpClient {
    ClientBuilder::new(client).with(TracingMiddleware).build()
}

#[allow(clippy::disallowed_methods)] // generic helper; echoagent CLI callers pass a policy-built client to traced_client
pub fn traced_client_new() -> TracedHttpClient {
    traced_client(reqwest::Client::new())
}

#[allow(clippy::disallowed_methods)] // generic middleware helper; callers supply a policy-built client
pub fn traced_client_from_builder(
    builder: reqwest::ClientBuilder,
) -> Result<TracedHttpClient, reqwest::Error> {
    Ok(traced_client(builder.build()?))
}

#[derive(Clone, Debug, Default)]
struct TracingMiddleware;

#[async_trait]
impl Middleware for TracingMiddleware {
    async fn handle(
        &self,
        mut req: reqwest::Request,
        extensions: &mut http::Extensions,
        next: reqwest_middleware::Next<'_>,
    ) -> reqwest_middleware::Result<reqwest::Response> {
        let method = req.method().as_str().to_owned();
        let url = req.url().clone();
        // No active dispatcher → the span has no consumer and would only be
        // downgraded to `log` spam. See `crate::dispatcher_active`.
        let span = if crate::dispatcher_active() {
            tracing::info_span!(
                "http_request",
                otel.kind = "client",
                "http.request.method" = %method,
                "url.full" = %url,
                "http.response.status_code" = field::Empty,
            )
        } else {
            Span::none()
        };

        let result = async move {
            attach_trace_to_http_request(req.headers_mut());
            next.run(req, extensions).await
        }
        .instrument(span.clone())
        .await;

        if let Ok(ref response) = result {
            span.record("http.response.status_code", response.status().as_u16());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{OtelTestEnv, otel_span_id_hex, otel_trace_id_hex, parse_traceparent};
    use tracing::Instrument;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn attach_trace_to_http_request_writes_traceparent() {
        let _env = OtelTestEnv::install();
        let span = tracing::info_span!("http_request", otel.kind = "client");
        let span_id = otel_span_id_hex(&span);
        let mut headers = HeaderMap::new();
        let _enter = span.enter();
        attach_trace_to_http_request(&mut headers);
        let tp = headers.get("traceparent").unwrap().to_str().unwrap();
        let (_ver, _tid, injected_span_id) = parse_traceparent(tp);
        assert_eq!(injected_span_id, span_id);
    }

    #[tokio::test]
    async fn traced_client_injects_client_span_not_parent_on_wire() {
        let _env = OtelTestEnv::install();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = traced_client(reqwest::Client::new());
        let parent = tracing::info_span!("parent_handler");
        let parent_span_id = otel_span_id_hex(&parent);
        let parent_trace_id = otel_trace_id_hex(&parent);

        async {
            client
                .get(format!("{}/health", server.uri()))
                .send()
                .await
                .unwrap();
        }
        .instrument(parent)
        .await;

        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        let tp = received[0]
            .headers
            .get("traceparent")
            .expect("traceparent on wire")
            .to_str()
            .unwrap();
        let (_ver, injected_trace_id, injected_span_id) = parse_traceparent(tp);

        assert_eq!(injected_trace_id, parent_trace_id);
        assert_ne!(injected_span_id, parent_span_id);
    }

    #[tokio::test]
    async fn traced_client_returns_response_status() {
        let _env = OtelTestEnv::install();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = traced_client_new();
        let resp = client
            .get(format!("{}/missing", server.uri()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 404);
        assert!(
            server.received_requests().await.unwrap()[0]
                .headers
                .get("traceparent")
                .is_some()
        );
    }
}
