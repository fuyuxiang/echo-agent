//! Checks that provider `query_params` and `env_http_headers` reach the
//! outgoing request.

mod support;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::{HeaderMap, Uri};
use axum::routing::post;
use echo_agent_sampler::SamplingClient;
use tokio::net::TcpListener;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_carries_query_params_and_env_http_headers() {
    // A unique name avoids clashing with other tests that read the process
    // environment; the surrounding whitespace exercises value trimming.
    let env_var = "ECHO_AGENT_SAMPLER_TEST_TENANT_TOKEN";
    unsafe { std::env::set_var(env_var, "  tenant-secret\n") };

    let captured: Arc<Mutex<Option<(String, HeaderMap)>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&captured);
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |uri: Uri, headers: HeaderMap| {
            let sink = Arc::clone(&sink);
            async move {
                *sink.lock().unwrap() =
                    Some((uri.query().unwrap_or_default().to_string(), headers));
                "{}"
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    // The base URL already carries `api-version`; a configured value must
    // replace it (not duplicate), keep unrelated keys, and percent-encode.
    let base_url = format!("http://{addr}/v1?api-version=old&keep=1");
    let mut cfg = support::test_config(&base_url, "test-key");
    cfg.query_params
        .insert("api-version".into(), "2026-07-22".into());
    cfg.query_params.insert("tenant".into(), "a b".into());
    cfg.env_http_headers
        .insert("x-tenant-token".into(), env_var.into());

    let client = SamplingClient::new(cfg).expect("client builds");
    support::send_one(&client).await;
    unsafe { std::env::remove_var(env_var) };

    let (query, headers) = captured.lock().unwrap().take().expect("request captured");
    assert_eq!(query.matches("api-version=").count(), 1, "query: {query}");
    assert!(query.contains("api-version=2026-07-22"), "query: {query}");
    assert!(!query.contains("api-version=old"), "query: {query}");
    assert!(query.contains("keep=1"), "query: {query}");
    assert!(
        query.contains("tenant=a%20b") || query.contains("tenant=a+b"),
        "query: {query}"
    );
    assert_eq!(headers.get("x-tenant-token").unwrap(), "tenant-secret");
}
