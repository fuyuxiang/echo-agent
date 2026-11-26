//! Axum-mock integration tests for [`crate::storage_client::StorageClient`]'s circuit breaker integration.

use super::{HttpUploadError, StorageClient, storage_breaker_config};
use axum::{Router, response::IntoResponse, routing::post};
use echo_agent_circuit_breaker::{BreakerState, Observer, Outcome};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::net::TcpListener;

/// Read the threshold straight from the preset so the test stays
/// in lock-step with `BreakerConfig::client()` if it ever changes.
fn storage_breaker_min_samples() -> u32 {
    storage_breaker_config().min_samples as u32
}

// 200 ms margin between cool-down and sleep keeps the timing
// tests stable on contended CI.
const TEST_OPEN_DURATION: Duration = Duration::from_millis(50);
const SLEEP_PAST_OPEN_DURATION: Duration = Duration::from_millis(250);

async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (addr, handle)
}

fn counted_handler(
    status: axum::http::StatusCode,
    counter: Arc<AtomicU32>,
) -> impl Fn() -> futures::future::Ready<axum::response::Response> + Clone {
    move || {
        counter.fetch_add(1, Ordering::Relaxed);
        futures::future::ready((status, "").into_response())
    }
}

fn client_short_open_duration(addr: SocketAddr, open_duration: Duration) -> StorageClient {
    StorageClient::new(&format!("http://{addr}/v1"), "test-token")
        .with_breaker_open_duration(open_duration)
}

async fn trip_breaker(client: &StorageClient) {
    let min_samples = storage_breaker_min_samples();
    for _ in 0..min_samples {
        let _ = client.upload("p", b"d", "text/plain").await;
    }
    assert!(
        client.storage_breaker_is_open(),
        "breaker must open after {min_samples} 401s",
    );
}

#[tokio::test]
async fn breaker_opens_after_threshold_401s() {
    let hits = Arc::new(AtomicU32::new(0));
    let router = Router::new().route(
        "/v1/storage",
        post(counted_handler(
            axum::http::StatusCode::UNAUTHORIZED,
            hits.clone(),
        )),
    );
    let (addr, _) = start_server(router).await;
    let client = client_short_open_duration(addr, Duration::from_secs(60));

    for _ in 0..storage_breaker_min_samples() {
        let err = client
            .upload("p", b"d", "text/plain")
            .await
            .expect_err("401 must surface as Err");
        let http_err = err
            .downcast_ref::<HttpUploadError>()
            .expect("err must be HttpUploadError");
        assert_eq!(http_err.status_code, 401);
    }
    assert_eq!(hits.load(Ordering::Relaxed), storage_breaker_min_samples());
    assert!(client.storage_breaker_is_open());

    // Subsequent calls must short-circuit.
    for _ in 0..10 {
        let err = client
            .upload("p", b"d", "text/plain")
            .await
            .expect_err("breaker-open must short-circuit as Err");
        let http_err = err
            .downcast_ref::<HttpUploadError>()
            .expect("short-circuit err must be HttpUploadError");
        assert_eq!(http_err.status_code, 503);
        assert!(
            http_err.message.contains("circuit breaker open"),
            "short-circuit message must mention circuit breaker, got: {}",
            http_err.message
        );
    }
    assert_eq!(
        hits.load(Ordering::Relaxed),
        storage_breaker_min_samples(),
        "server must not see any post-trip requests"
    );
}

/// Sliding-window sanity: a 200/401 mix below the failure-rate
/// threshold must NOT trip, even with enough samples to satisfy
/// `min_samples`. With `client()` preset (min_samples=5,
/// error_rate_threshold=0.5), 6 × 200 + 4 × 401 = 10 samples,
/// rate = 0.4 < 0.5 → still closed.  The successes lead so the
/// partial rate never crosses 0.5 once `min_samples` is reached.
#[tokio::test]
async fn sliding_window_below_threshold_does_not_trip() {
    let hits = Arc::new(AtomicU32::new(0));
    let hits_handler = hits.clone();
    let router = Router::new().route(
        "/v1/storage",
        post(move || {
            let n = hits_handler.fetch_add(1, Ordering::Relaxed);
            async move {
                if n < 6 {
                    axum::Json(serde_json::json!({
                        "bucket": "b",
                        "path": "p",
                        "size": 1,
                        "content_type": "text/plain",
                        "generation": 1
                    }))
                    .into_response()
                } else {
                    (axum::http::StatusCode::UNAUTHORIZED, "").into_response()
                }
            }
        }),
    );
    let (addr, _) = start_server(router).await;
    let client = client_short_open_duration(addr, Duration::from_secs(60));

    for _ in 0..10 {
        let _ = client.upload("p", b"d", "text/plain").await;
    }
    assert!(!client.storage_breaker_is_open());
    assert_eq!(hits.load(Ordering::Relaxed), 10);
}

#[tokio::test]
async fn breaker_half_open_after_cool_down_success() {
    // Server: first N requests 401 (trip), rest 200 (probe).
    let hits = Arc::new(AtomicU32::new(0));
    let hits_handler = hits.clone();
    let router = Router::new().route(
        "/v1/storage",
        post(move || {
            let n = hits_handler.fetch_add(1, Ordering::Relaxed);
            async move {
                if n < storage_breaker_min_samples() {
                    (axum::http::StatusCode::UNAUTHORIZED, "").into_response()
                } else {
                    axum::Json(serde_json::json!({
                        "bucket": "b",
                        "path": "p",
                        "size": 1,
                        "content_type": "text/plain",
                        "generation": 1
                    }))
                    .into_response()
                }
            }
        }),
    );
    let (addr, _) = start_server(router).await;
    let client = client_short_open_duration(addr, TEST_OPEN_DURATION);

    trip_breaker(&client).await;

    // Within cool-down: short-circuit.
    let _ = client.upload("p", b"d", "text/plain").await;
    assert_eq!(hits.load(Ordering::Relaxed), storage_breaker_min_samples());

    // Past cool-down: exactly one probe reaches the server.
    tokio::time::sleep(SLEEP_PAST_OPEN_DURATION).await;
    let probe = client.upload("p", b"d", "text/plain").await;
    assert!(probe.is_ok());
    assert_eq!(
        hits.load(Ordering::Relaxed),
        storage_breaker_min_samples() + 1
    );
    assert!(!client.storage_breaker_is_open());

    // Closed: subsequent calls go through normally.
    let _ = client.upload("p", b"d", "text/plain").await;
    assert_eq!(
        hits.load(Ordering::Relaxed),
        storage_breaker_min_samples() + 2
    );
}

#[tokio::test]
async fn breaker_half_open_after_cool_down_failure_reopens() {
    let hits = Arc::new(AtomicU32::new(0));
    let router = Router::new().route(
        "/v1/storage",
        post(counted_handler(
            axum::http::StatusCode::UNAUTHORIZED,
            hits.clone(),
        )),
    );
    let (addr, _) = start_server(router).await;
    let client = client_short_open_duration(addr, TEST_OPEN_DURATION);

    trip_breaker(&client).await;
    let baseline = hits.load(Ordering::Relaxed);

    // Past cool-down: probe 401s.
    tokio::time::sleep(SLEEP_PAST_OPEN_DURATION).await;
    let probe = client.upload("p", b"d", "text/plain").await;
    assert!(probe.is_err());
    assert_eq!(hits.load(Ordering::Relaxed), baseline + 1);
    assert!(client.storage_breaker_is_open());

    // Restarted cool-down: next call short-circuits.
    let after_probe = hits.load(Ordering::Relaxed);
    let _ = client.upload("p", b"d", "text/plain").await;
    assert_eq!(hits.load(Ordering::Relaxed), after_probe);
}

/// Breaker-open short-circuits surface `HttpUploadError { status_code: 503, .. }`
/// so they classify as retryable (retry with backoff) rather than as an auth
/// 401, keeping them distinct from the wire-401 path.
#[tokio::test]
async fn breaker_short_circuit_returns_http_upload_error_503() {
    let hits = Arc::new(AtomicU32::new(0));
    let router = Router::new().route(
        "/v1/storage",
        post(counted_handler(
            axum::http::StatusCode::UNAUTHORIZED,
            hits.clone(),
        )),
    );
    let (addr, _) = start_server(router).await;
    let client = client_short_open_duration(addr, Duration::from_secs(60));

    trip_breaker(&client).await;

    let err = client
        .upload("p", b"d", "text/plain")
        .await
        .expect_err("short-circuit must Err");
    let http_err = err
        .downcast_ref::<HttpUploadError>()
        .expect("short-circuit err must be HttpUploadError");
    assert_eq!(http_err.status_code, 503);
    assert!(http_err.message.contains("circuit breaker open"));
}

/// Concurrent half-open probes: N simultaneous `upload`s past the
/// cool-down must collapse into exactly ONE wire request. The
/// probe is held on a `Notify` so the breaker can't close before
/// the lagging callers race through `breaker.check()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn breaker_half_open_serialises_concurrent_probes() {
    let hits = Arc::new(AtomicU32::new(0));
    let probe_gate = Arc::new(tokio::sync::Notify::new());
    let probe_started = Arc::new(tokio::sync::Notify::new());

    let hits_handler = hits.clone();
    let probe_gate_handler = probe_gate.clone();
    let probe_started_handler = probe_started.clone();
    let router = Router::new().route(
        "/v1/storage",
        post(move || {
            let hits = hits_handler.clone();
            let gate = probe_gate_handler.clone();
            let started = probe_started_handler.clone();
            async move {
                let n = hits.fetch_add(1, Ordering::Relaxed);
                if n < storage_breaker_min_samples() {
                    (axum::http::StatusCode::UNAUTHORIZED, "").into_response()
                } else {
                    // Park the probe until the test releases it.
                    started.notify_one();
                    gate.notified().await;
                    axum::Json(serde_json::json!({
                        "bucket": "b",
                        "path": "p",
                        "size": 1,
                        "content_type": "text/plain",
                        "generation": 1
                    }))
                    .into_response()
                }
            }
        }),
    );
    let (addr, _) = start_server(router).await;
    let client = client_short_open_duration(addr, TEST_OPEN_DURATION);

    trip_breaker(&client).await;
    let hits_after_trip = hits.load(Ordering::Relaxed);
    assert_eq!(hits_after_trip, storage_breaker_min_samples());

    tokio::time::sleep(SLEEP_PAST_OPEN_DURATION).await;

    const N: usize = 16;
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    // `laggers_done` fires exactly once, when the (N-1) callers
    // that did NOT win the probe slot have surfaced from
    // `upload()` with a short-circuited `Err`. The probe task is
    // still parked in the server handler at that point, so it
    // has NOT yet been counted. Replacing the previous
    // 100 ms wall-clock sleep removes the CI-flake window where
    // a slow lagger could race the probe-gate release.
    let laggers_done = Arc::new(tokio::sync::Notify::new());
    let laggers_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::with_capacity(N);
    for _ in 0..N {
        let c = client.clone();
        let b = barrier.clone();
        let done = laggers_done.clone();
        let count = laggers_count.clone();
        tasks.push(tokio::spawn(async move {
            b.wait().await;
            let result = c.upload("p", b"d", "text/plain").await;
            let prev = count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            if prev + 1 == N - 1 {
                done.notify_one();
            }
            result
        }));
    }

    // Hold the probe until (a) the probe has reached the server,
    // and (b) all N-1 lagging callers have raced through
    // `breaker.check()` and short-circuited. Only then release
    // the gate so the probe can return 200 and close the breaker.
    probe_started.notified().await;
    laggers_done.notified().await;
    probe_gate.notify_one();

    for t in tasks {
        let _ = t.await.unwrap();
    }

    let probes = hits.load(Ordering::Relaxed) - hits_after_trip;
    assert_eq!(probes, 1, "exactly one probe must escape, saw {probes}");
    assert!(!client.storage_breaker_is_open());
}

/// Recording observer used to verify exactly one Open transition
/// and one Open→Closed close-via-probe transition.
#[derive(Default)]
struct RecordingObserver {
    transitions: Mutex<Vec<(BreakerState, BreakerState)>>,
}

impl Observer for RecordingObserver {
    fn on_state_change(&self, old: BreakerState, new: BreakerState, _reason: &str) {
        self.transitions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((old, new));
    }
    fn on_probe_admission(&self, _allowed: bool) {}
    fn on_outcome(&self, _outcome: Outcome, _state: BreakerState) {}
}

/// Exactly one open transition per open and one close-via-probe
/// transition per close, even when many wire-401s arrive while the
/// breaker is already open.
#[tokio::test]
async fn breaker_emits_exactly_one_warn_on_open_and_one_info_on_close() {
    let hits = Arc::new(AtomicU32::new(0));
    let hits_handler = hits.clone();
    let router = Router::new().route(
        "/v1/storage",
        post(move || {
            let n = hits_handler.fetch_add(1, Ordering::Relaxed);
            async move {
                if n < storage_breaker_min_samples() {
                    (axum::http::StatusCode::UNAUTHORIZED, "").into_response()
                } else {
                    axum::Json(serde_json::json!({
                        "bucket": "b",
                        "path": "p",
                        "size": 1,
                        "content_type": "text/plain",
                        "generation": 1
                    }))
                    .into_response()
                }
            }
        }),
    );
    let (addr, _) = start_server(router).await;
    let observer = Arc::new(RecordingObserver::default());
    let client = StorageClient::new(&format!("http://{addr}/v1"), "test-token")
        .with_breaker_for_testing(TEST_OPEN_DURATION, observer.clone());

    // 10 attempts: 5 wire 401s open the breaker, 5 short-circuit.
    // Only ONE Closed→Open transition must fire.
    for _ in 0..10 {
        let _ = client.upload("p", b"d", "text/plain").await;
    }
    assert!(client.storage_breaker_is_open());

    // Half-open probe → success → close → one HalfOpen→Closed transition.
    tokio::time::sleep(SLEEP_PAST_OPEN_DURATION).await;
    let probe = client.upload("p", b"d", "text/plain").await;
    assert!(probe.is_ok());

    let transitions = observer.transitions.lock().unwrap();
    let to_open = transitions
        .iter()
        .filter(|(_, to)| *to == BreakerState::Open)
        .count();
    let to_closed = transitions
        .iter()
        .filter(|(from, to)| *from == BreakerState::HalfOpen && *to == BreakerState::Closed)
        .count();
    assert_eq!(to_open, 1, "exactly one open transition, saw {to_open}");
    assert_eq!(
        to_closed, 1,
        "exactly one close-via-probe transition, saw {to_closed}"
    );
}
