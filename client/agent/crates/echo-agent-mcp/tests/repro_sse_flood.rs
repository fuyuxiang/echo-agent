//! Black-box repro of rmcp's zero-backoff SSE reconnect loop (and of
//! `McpHttpClient` bounding it), against a fake MCP streamable-HTTP server
//! whose standing-GET behavior is the variable under test. Each GET the fake
//! server counts corresponds to one rmcp `WARN sse stream error: ...` line.
//!
//! Run with: cargo test -p echo-agent-mcp --test repro_sse_flood -- --nocapture

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures::StreamExt;
use serde_json::{Value, json};

use echo_agent_mcp::mcp_http_client::{McpHttpClient, WarnBudget};
use echo_agent_mcp::rmcp::ServiceExt;
use echo_agent_mcp::rmcp::transport::StreamableHttpClientTransport;
use echo_agent_mcp::rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;

/// Gap letting the first body chunk flush before the abort, so the client
/// sees a *body* death (rmcp's zero-backoff flood path) rather than a failed
/// request (rmcp's policy-backed path).
const FLUSH_GAP: Duration = Duration::from_millis(5);

/// How the fake server ends the standing GET stream.
#[derive(Clone, Copy)]
enum GetBehavior {
    /// 200, then the body is aborted mid-stream — the production failure
    /// mode ("error decoding response body" -> the WARN flood path).
    AbnormalBodyDeath,
    /// 200, then the stream stays open (a working MCP server).
    Healthy,
}

#[derive(Clone)]
struct ServerState {
    behavior: GetBehavior,
    gets: Arc<AtomicUsize>,
}

async fn handle_post(Json(req): Json<Value>) -> Response {
    match req["method"].as_str() {
        Some("initialize") => {
            let result = json!({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {
                    "protocolVersion": req["params"]["protocolVersion"],
                    "capabilities": {},
                    "serverInfo": {"name": "fake", "version": "0.0.0"},
                },
            });
            ([("mcp-session-id", "fake-session-1")], Json(result)).into_response()
        }
        Some("tools/list") => {
            let result = json!({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {"tools": [{"name": "echo", "inputSchema": {"type": "object"}}]},
            });
            Json(result).into_response()
        }
        // notifications/initialized and anything else.
        _ => StatusCode::ACCEPTED.into_response(),
    }
}

async fn handle_get(State(state): State<ServerState>) -> Response {
    state.gets.fetch_add(1, Ordering::Relaxed);
    let body = match state.behavior {
        GetBehavior::AbnormalBodyDeath => Body::from_stream(
            futures::stream::iter([Ok::<_, std::io::Error>(": partial\n".to_owned())]).chain(
                futures::stream::once(async {
                    tokio::time::sleep(FLUSH_GAP).await;
                    Err(std::io::Error::other("body aborted mid-stream"))
                }),
            ),
        ),
        GetBehavior::Healthy => {
            Body::from_stream(futures::stream::pending::<Result<String, std::io::Error>>())
        }
    };
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}

async fn spawn_fake_server(behavior: GetBehavior) -> (String, Arc<AtomicUsize>) {
    let gets = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new()
        .route("/mcp", get(handle_get).post(handle_post))
        .with_state(ServerState {
            behavior,
            gets: gets.clone(),
        });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/mcp"), gets)
}

/// The flood: body death on the standing GET -> zero-backoff reconnect loop,
/// one WARN per GET (set RUST_LOG=rmcp=warn to see them).
#[tokio::test(flavor = "multi_thread")]
async fn repro_zero_backoff_reconnect_flood() {
    let (url, gets) = spawn_fake_server(GetBehavior::AbnormalBodyDeath).await;
    let transport = StreamableHttpClientTransport::from_uri(url.as_str());
    let client = ().serve(transport).await.expect("handshake against fake server should succeed");

    const OBSERVE: Duration = Duration::from_secs(3);
    tokio::time::sleep(OBSERVE).await;
    let n = gets.load(Ordering::Relaxed);
    let _ = client.cancel().await;

    eprintln!(
        "[repro] {n} standing-GET reconnect attempts (= WARN log lines) in {OBSERVE:?} \
         = {:.0}/sec",
        n as f64 / OBSERVE.as_secs_f64()
    );
    // A backoff-respecting client would attempt ~3-5 in 3s; the bug produces
    // hundreds-to-thousands.
    assert!(
        n > 20,
        "expected a zero-backoff reconnect flood (>20 GETs in {OBSERVE:?}), got {n}; \
         if this FAILS with a small n, the rmcp loop got fixed - delete mcp_http_client.rs"
    );
}

/// The fix: same body-killing server, client wrapped in `McpHttpClient` —
/// the backoff schedule allows only a handful of reconnects.
#[tokio::test(flavor = "multi_thread")]
async fn throttled_client_bounds_the_flood() {
    let (url, gets) = spawn_fake_server(GetBehavior::AbnormalBodyDeath).await;
    let throttled = McpHttpClient::new(
        reqwest::Client::default(),
        "fake-server",
        WarnBudget::default(),
    );
    let transport = StreamableHttpClientTransport::with_client(
        throttled,
        StreamableHttpClientTransportConfig::with_uri(url.as_str()),
    );
    let client = ().serve(transport).await.expect("handshake against fake server should succeed");

    // Checkpoint: backoff must engage early (instant, instant, 0.5s => 3-4
    // GETs by 1.2s; an unthrottled client would be in the hundreds).
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let early = gets.load(Ordering::Relaxed);
    assert!(
        early <= 4,
        "backoff must engage early; got {early} GETs by 1.2s"
    );

    const OBSERVE: Duration = Duration::from_secs(4);
    tokio::time::sleep(OBSERVE - Duration::from_millis(1200)).await;
    let n = gets.load(Ordering::Relaxed);
    let _ = client.cancel().await;

    eprintln!("[repro] {n} throttled reconnect attempts in {OBSERVE:?} (schedule allows ~5-6)");
    assert!(
        (3..=8).contains(&n),
        "throttled reconnects should follow the backoff schedule (~5-6 in {OBSERVE:?}), got {n}"
    );
}

/// A working server through the throttled client: handshake + tools/list
/// succeed and the standing GET opens exactly once — wrapper invisible.
#[tokio::test(flavor = "multi_thread")]
async fn throttled_client_does_not_affect_healthy_server() {
    let (url, gets) = spawn_fake_server(GetBehavior::Healthy).await;
    let throttled = McpHttpClient::new(
        reqwest::Client::default(),
        "fake-server",
        WarnBudget::default(),
    );
    let client = ()
        .serve(StreamableHttpClientTransport::with_client(
            throttled,
            StreamableHttpClientTransportConfig::with_uri(url.as_str()),
        ))
        .await
        .expect("handshake against healthy server should succeed");

    let tools = client
        .list_tools(Default::default())
        .await
        .expect("tools/list should succeed through the throttled client");
    assert_eq!(tools.tools.len(), 1);
    assert_eq!(tools.tools[0].name, "echo");

    tokio::time::sleep(Duration::from_secs(3)).await;
    let n = gets.load(Ordering::Relaxed);
    let _ = client.cancel().await;

    eprintln!("[repro] healthy server: {n} GET(s) in 3s, tools/list ok");
    assert_eq!(
        n, 1,
        "healthy stream must be opened once and never reconnected"
    );
}
