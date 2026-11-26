//! Tests for StorageClient retry logic.
//!
//! Uses a local axum server to simulate various HTTP error scenarios
//! and verify that the client handles retries correctly.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use tokio::net::TcpListener;

use echo_agent_file_utils::storage_client::{RetryConfig, StorageClient};

/// Shared state for tracking request counts in tests.
#[derive(Clone, Default)]
struct TestServerState {
    request_count: Arc<AtomicU32>,
    /// Number of 429 responses to return before succeeding
    fail_count: Arc<AtomicU32>,
    /// Optional Retry-After header value in seconds
    retry_after_secs: Option<u32>,
}

impl TestServerState {
    fn new(fail_count: u32) -> Self {
        Self {
            request_count: Arc::new(AtomicU32::new(0)),
            fail_count: Arc::new(AtomicU32::new(fail_count)),
            retry_after_secs: None,
        }
    }

    fn with_retry_after(mut self, secs: u32) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }

    fn get_request_count(&self) -> u32 {
        self.request_count.load(Ordering::SeqCst)
    }
}

/// Handler that returns 429 for the first N requests, then succeeds.
async fn upload_handler_429(
    State(state): State<TestServerState>,
    _headers: HeaderMap,
    _body: Body,
) -> Response {
    let count = state.request_count.fetch_add(1, Ordering::SeqCst);
    let fail_count = state.fail_count.load(Ordering::SeqCst);

    if count < fail_count {
        let mut response = (
            StatusCode::TOO_MANY_REQUESTS,
            r#"{"error": "rate limited"}"#,
        )
            .into_response();

        // Add Retry-After header if configured
        if let Some(secs) = state.retry_after_secs {
            response
                .headers_mut()
                .insert("retry-after", secs.to_string().parse().unwrap());
        }

        return response;
    }

    // Success response
    (
        StatusCode::OK,
        r#"{"bucket": "test-bucket", "path": "test/path", "size": 100, "content_type": "application/json", "generation": 1}"#,
    )
        .into_response()
}

/// Handler that returns 500 for the first N requests, then succeeds.
async fn upload_handler_500(
    State(state): State<TestServerState>,
    _headers: HeaderMap,
    _body: Body,
) -> Response {
    let count = state.request_count.fetch_add(1, Ordering::SeqCst);
    let fail_count = state.fail_count.load(Ordering::SeqCst);

    if count < fail_count {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error": "internal server error"}"#,
        )
            .into_response();
    }

    // Success response
    (
        StatusCode::OK,
        r#"{"bucket": "test-bucket", "path": "test/path", "size": 100, "content_type": "application/json", "generation": 1}"#,
    )
        .into_response()
}

/// Handler that always returns 400 (non-retryable).
async fn upload_handler_400(
    State(state): State<TestServerState>,
    _headers: HeaderMap,
    _body: Body,
) -> Response {
    state.request_count.fetch_add(1, Ordering::SeqCst);

    (StatusCode::BAD_REQUEST, r#"{"error": "bad request"}"#).into_response()
}

/// Handler that always returns 429 (never succeeds).
async fn upload_handler_always_429(
    State(state): State<TestServerState>,
    _headers: HeaderMap,
    _body: Body,
) -> Response {
    state.request_count.fetch_add(1, Ordering::SeqCst);

    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        r#"{"error": "rate limited"}"#,
    )
        .into_response();

    if let Some(secs) = state.retry_after_secs {
        response
            .headers_mut()
            .insert("retry-after", secs.to_string().parse().unwrap());
    }

    response
}

/// Handler that always succeeds immediately.
async fn upload_handler_success(
    State(state): State<TestServerState>,
    _headers: HeaderMap,
    _body: Body,
) -> Response {
    state.request_count.fetch_add(1, Ordering::SeqCst);

    (
        StatusCode::OK,
        r#"{"bucket": "test-bucket", "path": "test/path", "size": 100, "content_type": "application/json", "generation": 1}"#,
    )
        .into_response()
}

/// Start a test server with the given handler and return its address.
async fn start_test_server<H, T>(state: TestServerState, handler: H) -> SocketAddr
where
    H: axum::handler::Handler<T, TestServerState> + Clone + Send + 'static,
    T: 'static,
{
    let app = Router::new()
        .route("/v1/storage", post(handler))
        .with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    // Give the server a moment to start
    tokio::time::sleep(Duration::from_millis(10)).await;

    addr
}

#[tokio::test]
async fn test_upload_succeeds_on_first_try() {
    let state = TestServerState::new(0);
    let addr = start_test_server(state.clone(), upload_handler_success).await;

    let client = StorageClient::new(&format!("http://{}/v1", addr), "test-token");

    let result = client
        .upload("test/path", b"test content", "text/plain")
        .await;

    assert!(result.is_ok());
    assert_eq!(state.get_request_count(), 1);
}

#[tokio::test]
async fn test_upload_retries_on_429() {
    let state = TestServerState::new(2); // Fail twice, then succeed
    let addr = start_test_server(state.clone(), upload_handler_429).await;

    let client = StorageClient::new(&format!("http://{}/v1", addr), "test-token")
        .with_retry_config(
            RetryConfig::new()
                .with_initial_delay(Duration::from_millis(10))
                .with_max_retries(5),
        );

    let result = client
        .upload("test/path", b"test content", "text/plain")
        .await;

    assert!(result.is_ok());
    assert_eq!(state.get_request_count(), 3); // 2 failures + 1 success
}

#[tokio::test]
async fn test_upload_retries_on_500() {
    let state = TestServerState::new(2); // Fail twice, then succeed
    let addr = start_test_server(state.clone(), upload_handler_500).await;

    let client = StorageClient::new(&format!("http://{}/v1", addr), "test-token")
        .with_retry_config(
            RetryConfig::new()
                .with_initial_delay(Duration::from_millis(10))
                .with_max_retries(5),
        );

    let result = client
        .upload("test/path", b"test content", "text/plain")
        .await;

    assert!(result.is_ok());
    assert_eq!(state.get_request_count(), 3); // 2 failures + 1 success
}

#[tokio::test]
async fn test_upload_does_not_retry_on_400() {
    let state = TestServerState::new(0);
    let addr = start_test_server(state.clone(), upload_handler_400).await;

    let client = StorageClient::new(&format!("http://{}/v1", addr), "test-token")
        .with_retry_config(
            RetryConfig::new()
                .with_initial_delay(Duration::from_millis(10))
                .with_max_retries(5),
        );

    let result = client
        .upload("test/path", b"test content", "text/plain")
        .await;

    assert!(result.is_err());
    assert_eq!(state.get_request_count(), 1); // Only 1 request, no retries
}

#[tokio::test]
async fn test_upload_respects_max_retries() {
    let state = TestServerState::new(100); // Always fail
    let addr = start_test_server(state.clone(), upload_handler_always_429).await;

    let client = StorageClient::new(&format!("http://{}/v1", addr), "test-token")
        .with_retry_config(
            RetryConfig::new()
                .with_initial_delay(Duration::from_millis(10))
                .with_max_retries(3),
        );

    let result = client
        .upload("test/path", b"test content", "text/plain")
        .await;

    assert!(result.is_err());
    // Should have 1 initial request + 3 retries = 4 total
    assert_eq!(state.get_request_count(), 4);
}

#[tokio::test]
async fn test_upload_respects_retry_after_header() {
    let state = TestServerState::new(1).with_retry_after(1); // 1 second Retry-After
    let addr = start_test_server(state.clone(), upload_handler_429).await;

    let client = StorageClient::new(&format!("http://{}/v1", addr), "test-token")
        .with_retry_config(
            RetryConfig::new()
                .with_initial_delay(Duration::from_millis(10))
                .with_max_retries(3),
        );

    let start = Instant::now();
    let result = client
        .upload("test/path", b"test content", "text/plain")
        .await;
    let elapsed = start.elapsed();

    assert!(result.is_ok());
    // Should have waited at least 1 second due to Retry-After header
    assert!(
        elapsed >= Duration::from_millis(900),
        "Expected delay >= 900ms due to Retry-After, got {:?}",
        elapsed
    );
}

#[tokio::test]
async fn test_exponential_backoff_increases_delay() {
    let state = TestServerState::new(3); // Fail 3 times, then succeed
    let addr = start_test_server(state.clone(), upload_handler_429).await;

    let client = StorageClient::new(&format!("http://{}/v1", addr), "test-token")
        .with_retry_config(
            RetryConfig::new()
                .with_initial_delay(Duration::from_millis(50))
                .with_multiplier(2.0)
                .with_jitter_factor(0.0) // No jitter for predictable timing
                .with_max_retries(5),
        );

    let start = Instant::now();
    let result = client
        .upload("test/path", b"test content", "text/plain")
        .await;
    let elapsed = start.elapsed();

    assert!(result.is_ok());

    // With 3 failures before success:
    // Delay 1: 50ms, Delay 2: 100ms, Delay 3: 200ms = 350ms minimum
    assert!(
        elapsed >= Duration::from_millis(300),
        "Expected delay >= 300ms for exponential backoff, got {:?}",
        elapsed
    );
}

// ============================================================================
