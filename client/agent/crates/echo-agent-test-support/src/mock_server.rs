//! Mock inference server. Logs every request and shuts down on drop.
//!
//! Serves the three inference endpoints (`/v1/chat/completions`,
//! `/v1/responses`, `/v1/messages`) plus `/v1/models`, `/v1/settings`,
//! `/v1/user`, `/v1/storage`, `/v1/privacy/coding-data-retention`, and
//! session writeback (`POST /sessions/{id}/data`, `PUT /sessions/{id}`).
//!
//! The inference endpoints answer from the first source that matches: a named
//! expectation, then the path's [`ScriptedResponse`] queue, then the active
//! mode, which echoes the last user message until
//! [`MockInferenceServer::set_response`] replaces it with a fixed text.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use anyhow::Context as _;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use futures_util::stream;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::inference_override::{ClassifiedInferenceRequest, InferenceOverrides};
pub use crate::inference_override::{
    InferenceEndpoint, InferenceExpectation, InferenceRequestMatcher,
};
use crate::scripted::TerminalWait;
pub use crate::scripted::{ScriptedBody, ScriptedResponse, SseEvent};
use crate::sse;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub method: String,
    pub path: String,
    pub body: Option<Value>,
    pub authorization: Option<String>,
    /// Lowercase names in arrival order. Empty for the GET endpoints.
    pub headers: Vec<(String, String)>,
    /// Wall-clock arrival time, for latency-harness request timelines.
    pub at: std::time::SystemTime,
}

impl LogEntry {
    /// First value of `name` (case-insensitive), if the request carried it.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// An entry holds a whole conversation, so the log evicts oldest first.
const MAX_LOGGED_REQUESTS: usize = 1024;

pub struct RequestLog {
    count: AtomicU32,
    entries: std::sync::Mutex<Vec<LogEntry>>,
    keep_entries: AtomicBool,
}

impl RequestLog {
    fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            entries: std::sync::Mutex::new(Vec::new()),
            keep_entries: AtomicBool::new(true),
        }
    }

    fn record(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        authorization: Option<&str>,
        headers: Vec<(String, String)>,
    ) {
        self.count.fetch_add(1, Ordering::SeqCst);
        if !self.keep_entries.load(Ordering::SeqCst) {
            return;
        }
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= MAX_LOGGED_REQUESTS {
            entries.remove(0);
        }
        entries.push(LogEntry {
            method: method.to_string(),
            path: path.to_string(),
            body: body.cloned(),
            authorization: authorization.map(String::from),
            headers,
            at: std::time::SystemTime::now(),
        });
    }
}

/// A model served by `/v1/models`. Each field is emitted under its camelCase
/// name when set, at the top level except for `agent_type`, which goes in
/// `_meta`.
#[derive(Debug, Clone)]
pub struct MockModelEntry {
    pub id: String,
    pub agent_type: Option<String>,
    pub api_backend: Option<String>,
    pub supports_backend_search: bool,
    pub supports_reasoning_effort: bool,
    pub reasoning_effort: Option<String>,
    /// Each entry is a table carrying a `value` key, or a bare value string.
    /// `parse_remote_model_value` defines the full shape.
    pub reasoning_efforts: Vec<Value>,
}

impl MockModelEntry {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            agent_type: None,
            api_backend: None,
            supports_backend_search: false,
            supports_reasoning_effort: false,
            reasoning_effort: None,
            reasoning_efforts: Vec::new(),
        }
    }

    pub fn with_agent_type(id: impl Into<String>, agent_type: impl Into<String>) -> Self {
        Self {
            agent_type: Some(agent_type.into()),
            ..Self::new(id)
        }
    }

    pub fn with_api_backend(mut self, api_backend: impl Into<String>) -> Self {
        self.api_backend = Some(api_backend.into());
        self
    }

    pub fn with_supports_backend_search(mut self, supports: bool) -> Self {
        self.supports_backend_search = supports;
        self
    }

    pub fn with_supports_reasoning_effort(mut self, supports: bool) -> Self {
        self.supports_reasoning_effort = supports;
        self
    }

    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    pub fn with_reasoning_efforts(mut self, efforts: Vec<Value>) -> Self {
        self.reasoning_efforts = efforts;
        self
    }

    fn to_json(&self) -> Value {
        let mut obj = json!({
            "id": self.id,
            "object": "model",
            "created": 1234567890,
            "owned_by": "test"
        });
        if let Some(ref at) = self.agent_type {
            obj["_meta"] = json!({ "agentType": at });
        }
        if let Some(ref backend) = self.api_backend {
            obj["apiBackend"] = json!(backend);
        }
        if self.supports_backend_search {
            obj["supportsBackendSearch"] = json!(true);
        }
        if self.supports_reasoning_effort {
            obj["supportsReasoningEffort"] = json!(true);
        }
        if let Some(ref effort) = self.reasoning_effort {
            obj["reasoningEffort"] = json!(effort);
        }
        if !self.reasoning_efforts.is_empty() {
            obj["reasoningEfforts"] = json!(self.reasoning_efforts);
        }
        obj
    }
}

enum ResponseMode {
    /// `Echo: <last user message>`, with whitespace collapsed.
    Echo,
    /// Deltas reconstruct the text byte for byte, newlines included.
    Fixed(String),
}

/// Emit each SSE event after `delay` and optionally wait before the final one.
fn paced_events(
    events: Vec<axum::response::sse::Event>,
    delay: Option<Duration>,
    before_terminal: Option<TerminalWait>,
) -> impl futures_util::Stream<Item = Result<axum::response::sse::Event, Infallible>> {
    let last_idx = events.len().checked_sub(1);
    stream::unfold(
        (events.into_iter().enumerate(), before_terminal),
        move |(mut events, mut before_terminal)| async move {
            let (idx, event) = events.next()?;
            if let Some(d) = delay {
                tokio::time::sleep(d).await;
            }
            if Some(idx) == last_idx
                && let Some(wait) = before_terminal.take()
            {
                wait().await;
            }
            Some((Ok::<_, Infallible>(event), (events, before_terminal)))
        },
    )
}

const STORAGE_BODY_CAPTURE_CAP: usize = 256 * 1024;

/// An upload `/v1/storage` accepted.
#[derive(Debug, Clone)]
pub struct StorageUpload {
    pub path: String,
    pub size: usize,
    /// Empty when `size` exceeds `STORAGE_BODY_CAPTURE_CAP`.
    pub body: Vec<u8>,
    pub authorization: Option<String>,
}

/// A 401 gate tests can flip, plus the uploads accepted through it.
#[derive(Default)]
struct StorageState {
    unauthorized: AtomicBool,
    request_count: AtomicU32,
    uploads: std::sync::Mutex<Vec<StorageUpload>>,
}

pub struct MockInferenceServer {
    addr: SocketAddr,
    shutdown_tx: Option<oneshot::Sender<()>>,
    log: Arc<RequestLog>,
    models: Arc<std::sync::RwLock<Vec<Value>>>,
    settings: Arc<std::sync::RwLock<Option<Value>>>,
    response_mode: Arc<std::sync::RwLock<ResponseMode>>,
    overrides: InferenceOverrides,
    /// One assistant text per agent turn, consumed in order.
    agent_turns: Arc<std::sync::Mutex<VecDeque<String>>>,
    /// `stop_reason` on the `/v1/messages` terminal `message_delta`.
    messages_stop_reason: Arc<std::sync::RwLock<String>>,
    chunk_delay: Arc<std::sync::RwLock<Option<Duration>>>,
    storage: Arc<StorageState>,
    /// When set, `/v1/models` and `/v1/settings` never respond.
    hang: Arc<std::sync::atomic::AtomicBool>,
    user_tier: Arc<std::sync::RwLock<Option<String>>>,
}

impl MockInferenceServer {
    /// Serves one `test-model` with no agent type.
    pub async fn start() -> anyhow::Result<Self> {
        Self::start_with_models(vec![MockModelEntry::new("test-model")]).await
    }

    pub async fn start_with_models(models: Vec<MockModelEntry>) -> anyhow::Result<Self> {
        Self::start_inner(models, None).await
    }

    /// Start a mock that returns 401 on inference requests missing
    /// `Authorization: Bearer <required_token>`.
    pub async fn start_with_required_auth(
        models: Vec<MockModelEntry>,
        required_token: impl Into<String>,
    ) -> anyhow::Result<Self> {
        Self::start_inner(models, Some(required_token.into())).await
    }

    async fn start_inner(
        models: Vec<MockModelEntry>,
        required_token: Option<String>,
    ) -> anyhow::Result<Self> {
        let log = Arc::new(RequestLog::new());
        let models_json: Vec<Value> = models.iter().map(MockModelEntry::to_json).collect();
        let shared_models = Arc::new(std::sync::RwLock::new(models_json));
        let shared_settings = Arc::new(std::sync::RwLock::new(None::<Value>));
        let response_mode = Arc::new(std::sync::RwLock::new(ResponseMode::Echo));
        let overrides = InferenceOverrides::new(required_token);
        let agent_turns = Arc::new(std::sync::Mutex::new(VecDeque::new()));
        let messages_stop_reason = Arc::new(std::sync::RwLock::new("end_turn".to_string()));
        let chunk_delay = Arc::new(std::sync::RwLock::new(None::<Duration>));
        let storage = Arc::new(StorageState::default());
        let hang = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let user_tier = Arc::new(std::sync::RwLock::new(None::<String>));
        let app = Self::build_router(
            log.clone(),
            shared_models.clone(),
            shared_settings.clone(),
            response_mode.clone(),
            overrides.clone(),
            agent_turns.clone(),
            messages_stop_reason.clone(),
            chunk_delay.clone(),
            storage.clone(),
            hang.clone(),
            user_tier.clone(),
        );

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("bind mock inference server")?;
        let addr = listener.local_addr().context("local_addr")?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = shutdown_rx.await;
                })
                .await
                .unwrap();
        });

        // Wait for server readiness — try connecting instead of a fixed sleep.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::net::TcpStream::connect(addr).await.is_err() {
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!("mock server not ready within 5s");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        Ok(Self {
            addr,
            shutdown_tx: Some(shutdown_tx),
            log,
            models: shared_models,
            settings: shared_settings,
            response_mode,
            overrides,
            agent_turns,
            messages_stop_reason,
            chunk_delay,
            storage,
            hang,
            user_tier,
        })
    }

    pub fn set_models(&self, models: Vec<MockModelEntry>) {
        let mut guard = self.models.write().unwrap();
        *guard = models.iter().map(MockModelEntry::to_json).collect();
    }

    /// Stream this text instead of echoing. Deltas reconstruct it byte for byte.
    pub fn set_response(&self, text: impl Into<String>) {
        *self.response_mode.write().unwrap() = ResponseMode::Fixed(text.into());
    }

    /// Consumed FIFO per `path`, e.g. `"/v1/chat/completions"`. An empty queue
    /// falls back to the response mode.
    pub fn enqueue_response(&self, path: impl Into<String>, response: ScriptedResponse) {
        self.overrides.enqueue_response(path, response);
    }

    /// Default-responder concurrency cap: over `cap` in-flight (each held for
    /// `hold`), extra requests get 429 + `Retry-After`. Scripts/expectations bypass.
    pub fn set_inference_concurrency_cap(&self, cap: usize, hold: Duration, retry_after_secs: u64) {
        self.overrides
            .set_concurrency_cap(cap, hold, retry_after_secs);
    }

    /// Register one named response matched atomically by endpoint and request kind.
    #[must_use = "keep the handle to synchronize and assert expectation satisfaction"]
    pub fn expect_response(
        &self,
        name: impl Into<String>,
        matcher: InferenceRequestMatcher,
        response: ScriptedResponse,
    ) -> InferenceExpectation {
        self.overrides
            .register_expectation(name, matcher, response, false)
    }

    /// Register a named response that pauses immediately before completion.
    #[must_use = "keep the handle to release and assert expectation satisfaction"]
    pub fn expect_response_blocked(
        &self,
        name: impl Into<String>,
        matcher: InferenceRequestMatcher,
        response: ScriptedResponse,
    ) -> InferenceExpectation {
        self.overrides
            .register_expectation(name, matcher, response, true)
    }

    /// Queue one byte-exact response per foreground turn.
    pub fn set_agent_turns(&self, turns: impl IntoIterator<Item = String>) {
        *self.agent_turns.lock().unwrap() = turns.into_iter().collect();
    }

    /// Until this is called, `GET /v1/settings` returns 404.
    pub fn set_settings(&self, settings: impl serde::Serialize) {
        let value = serde_json::to_value(settings).expect("serialize settings");
        let mut guard = self.settings.write().unwrap();
        *guard = Some(value);
    }

    /// The smallest settings payload that opens the subscription gate. Without
    /// it a client sits on the upsell screen.
    pub fn preset_allow_access(&self) {
        self.set_settings(json!({ "allow_access": true }));
    }

    /// Stand in for a black-holed backend.
    pub fn set_hang(&self, hang: bool) {
        self.hang.store(hang, std::sync::atomic::Ordering::Release);
    }

    /// The `subscriptionTier` on `GET /v1/user`. `None`, the default, omits the
    /// field, which the shell reads as the free tier.
    pub fn set_user_subscription_tier(&self, tier: Option<&str>) {
        *self.user_tier.write().unwrap() = tier.map(str::to_owned);
    }

    /// Defaults to `"end_turn"`.
    pub fn set_messages_stop_reason(&self, stop_reason: impl Into<String>) {
        *self.messages_stop_reason.write().unwrap() = stop_reason.into();
    }

    /// Emit each SSE event after `delay`, so a test can hold a turn visibly
    /// streaming. `None` restores instant streaming. Applies to requests
    /// started after the call.
    pub fn set_chunk_delay(&self, delay: Option<Duration>) {
        *self.chunk_delay.write().unwrap() = delay;
    }

    /// Hold foreground terminal SSE events until
    /// [`Self::release_agent_completions`]. Prefer per-expectation blocking.
    pub fn hold_agent_completions(&self) {
        self.overrides.hold_completions();
    }

    /// Let held and future agent turns emit their terminal event.
    pub fn release_agent_completions(&self) {
        self.overrides.release_completions();
    }

    /// e.g. `http://127.0.0.1:12345/v1`
    pub fn url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Origin without the `/v1` inference prefix (`http://127.0.0.1:PORT`).
    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn request_count(&self) -> u32 {
        self.log.count.load(Ordering::SeqCst)
    }

    /// Stop retaining entries. [`Self::request_count`] stays exact.
    pub fn set_keep_requests(&self, enabled: bool) {
        self.log.keep_entries.store(enabled, Ordering::SeqCst);
    }

    pub fn requests(&self) -> Vec<LogEntry> {
        self.log.entries.lock().unwrap().clone()
    }

    /// Bodies of all received requests, in arrival order (body-less requests
    /// such as `GET /v1/models` are skipped).
    pub fn request_bodies(&self) -> Vec<Value> {
        self.log
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| e.body.clone())
            .collect()
    }

    pub fn has_chat_completion_request(&self) -> bool {
        self.log
            .entries
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.path.contains("chat/completions"))
    }

    pub fn has_responses_request(&self) -> bool {
        self.log
            .entries
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.path.contains("responses"))
    }

    /// Number of `POST /v1/messages` requests received so far.
    pub fn messages_request_count(&self) -> usize {
        self.log
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.path == "/v1/messages")
            .count()
    }

    /// Format the request log for diagnostic output on test failures.
    pub fn request_log_summary(&self) -> String {
        let entries = self.log.entries.lock().unwrap();
        if entries.is_empty() {
            return "(no requests received)".to_string();
        }
        entries
            .iter()
            .enumerate()
            .map(|(i, e)| format!("  [{}] {} {}", i, e.method, e.path))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Get the system prompt from the most recent inference request.
    pub fn last_system_prompt(&self) -> Option<String> {
        let entries = self.log.entries.lock().unwrap();
        entries
            .iter()
            .rev()
            .find(|e| e.path.contains("chat/completions") || e.path.contains("responses"))
            .and_then(|e| e.body.as_ref())
            .and_then(|body| {
                // Chat completions format: messages[0].content (system message)
                body.get("messages")
                    .and_then(|m| m.as_array())
                    .and_then(|msgs| msgs.first())
                    .and_then(|msg| msg.get("content"))
                    .and_then(|c| c.as_str())
                    .map(String::from)
                    // Responses API format: instructions field
                    .or_else(|| {
                        body.get("instructions")
                            .and_then(|s| s.as_str())
                            .map(String::from)
                    })
            })
    }

    /// While closed, every `/v1/storage` upload is rejected with 401.
    pub fn set_storage_unauthorized(&self, unauthorized: bool) {
        self.storage
            .unauthorized
            .store(unauthorized, Ordering::SeqCst);
    }

    /// Total `/v1/storage` upload attempts seen, including 401-rejected ones.
    pub fn storage_request_count(&self) -> u32 {
        self.storage.request_count.load(Ordering::SeqCst)
    }

    /// Only the uploads that were accepted.
    pub fn storage_uploads(&self) -> Vec<StorageUpload> {
        self.storage.uploads.lock().unwrap().clone()
    }

    /// Counts the attempt, then either rejects it or records it and answers in
    /// the proxy's `UploadResponse` shape.
    fn storage_upload_handler(
        storage: &StorageState,
        headers: &HeaderMap,
        body: &axum::body::Bytes,
    ) -> Response {
        storage.request_count.fetch_add(1, Ordering::SeqCst);
        if storage.unauthorized.load(Ordering::SeqCst) {
            return (
                StatusCode::UNAUTHORIZED,
                r#"{"error":"Invalid or expired credentials (mock)"}"#,
            )
                .into_response();
        }

        let path = headers
            .get("X-Storage-Path")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let size = body.len();
        let captured_body = if size <= STORAGE_BODY_CAPTURE_CAP {
            body.to_vec()
        } else {
            Vec::new()
        };
        let authorization = Self::extract_auth(headers);
        let response = (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({
                "bucket": "mock-bucket",
                "path": path,
                "size": size,
                "content_type": "application/octet-stream",
                "generation": 1,
            })
            .to_string(),
        );
        storage.uploads.lock().unwrap().push(StorageUpload {
            path,
            size,
            body: captured_body,
            authorization,
        });
        response.into_response()
    }

    fn extract_auth(headers: &HeaderMap) -> Option<String> {
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    }

    fn headers_vec(headers: &HeaderMap) -> Vec<(String, String)> {
        headers
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    String::from_utf8_lossy(v.as_bytes()).into_owned(),
                )
            })
            .collect()
    }

    fn pop_agent_turn(
        agent_turns: &Arc<std::sync::Mutex<VecDeque<String>>>,
        request: &ClassifiedInferenceRequest,
    ) -> Option<String> {
        if !request.is_foreground() {
            return None;
        }
        agent_turns.lock().unwrap().pop_front()
    }

    fn build_router(
        log: Arc<RequestLog>,
        models: Arc<std::sync::RwLock<Vec<Value>>>,
        settings: Arc<std::sync::RwLock<Option<Value>>>,
        response_mode: Arc<std::sync::RwLock<ResponseMode>>,
        overrides: InferenceOverrides,
        agent_turns: Arc<std::sync::Mutex<VecDeque<String>>>,
        messages_stop_reason: Arc<std::sync::RwLock<String>>,
        chunk_delay: Arc<std::sync::RwLock<Option<Duration>>>,
        storage: Arc<StorageState>,
        hang: Arc<std::sync::atomic::AtomicBool>,
        user_tier: Arc<std::sync::RwLock<Option<String>>>,
    ) -> Router {
        let hang_models = hang.clone();
        let hang_settings = hang;
        let log_cc = log.clone();
        let log_rs = log.clone();
        let log_msg = log.clone();
        let log_session_data = log.clone();
        let log_session_upsert = log.clone();
        let overrides_session_data = overrides.clone();
        let overrides_session_upsert = overrides.clone();
        let mode_cc = response_mode.clone();
        let mode_rs = response_mode.clone();
        let mode_msg = response_mode;
        let overrides_cc = overrides.clone();
        let overrides_rs = overrides.clone();
        let overrides_settings = overrides.clone();
        let overrides_msg = overrides;
        let agent_turns_cc = agent_turns.clone();
        let agent_turns_rs = agent_turns.clone();
        let agent_turns_msg = agent_turns;
        let delay_cc = chunk_delay.clone();
        let delay_rs = chunk_delay.clone();
        let delay_msg = chunk_delay;

        Router::new()
            .route(
                "/v1/chat/completions",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let log = log_cc.clone();
                    let mode = mode_cc.clone();
                    let overrides = overrides_cc.clone();
                    let agent_turns = agent_turns_cc.clone();
                    let delay = delay_cc.clone();
                    async move {
                        let auth = Self::extract_auth(&headers);
                        log.record(
                            "POST",
                            "/v1/chat/completions",
                            Some(&body),
                            auth.as_deref(),
                            Self::headers_vec(&headers),
                        );

                        let request =
                            overrides.classify(InferenceEndpoint::ChatCompletions, &headers, &body);
                        let chunk_delay = *delay.read().unwrap();
                        if let Some(response) = overrides
                            .response_override(&request, &headers, chunk_delay)
                            .await
                        {
                            return response;
                        }

                        let user_msg = body
                            .get("messages")
                            .and_then(|m| m.as_array())
                            .and_then(|msgs| {
                                msgs.iter()
                                    .rev()
                                    .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
                            })
                            .and_then(|m| m.get("content"))
                            .and_then(Value::as_str)
                            .unwrap_or("hello");

                        let model = body
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or("test-model");

                        let events = match Self::pop_agent_turn(&agent_turns, &request) {
                            Some(text) => sse::chat_completion_events_exact(&text, model),
                            None => match &*mode.read().unwrap() {
                                ResponseMode::Echo => {
                                    sse::chat_completion_events(&format!("Echo: {user_msg}"), model)
                                }
                                ResponseMode::Fixed(text) => {
                                    sse::chat_completion_events_exact(text, model)
                                }
                            },
                        };
                        let gate = overrides.fallback_terminal_wait(&request);
                        let stream = paced_events(events, *delay.read().unwrap(), gate);
                        Sse::new(stream)
                            .keep_alive(KeepAlive::default())
                            .into_response()
                    }
                }),
            )
            .route(
                "/v1/responses",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let log = log_rs.clone();
                    let mode = mode_rs.clone();
                    let overrides = overrides_rs.clone();
                    let agent_turns = agent_turns_rs.clone();
                    let delay = delay_rs.clone();
                    async move {
                        let auth = Self::extract_auth(&headers);
                        log.record(
                            "POST",
                            "/v1/responses",
                            Some(&body),
                            auth.as_deref(),
                            Self::headers_vec(&headers),
                        );

                        let request =
                            overrides.classify(InferenceEndpoint::Responses, &headers, &body);
                        let chunk_delay = *delay.read().unwrap();
                        if let Some(response) = overrides
                            .response_override(&request, &headers, chunk_delay)
                            .await
                        {
                            return response;
                        }

                        let user_msg = body
                            .get("input")
                            .and_then(|i| i.as_array())
                            .and_then(|items| {
                                items.iter().rev().find(|item| {
                                    item.get("role").and_then(Value::as_str) == Some("user")
                                })
                            })
                            .and_then(|item| {
                                item.get("content").and_then(|c| {
                                    c.as_str().map(String::from).or_else(|| {
                                        c.as_array().and_then(|parts| {
                                            parts.iter().find_map(|p| {
                                                if p.get("type").and_then(Value::as_str)
                                                    == Some("input_text")
                                                {
                                                    p.get("text")
                                                        .and_then(Value::as_str)
                                                        .map(String::from)
                                                } else {
                                                    None
                                                }
                                            })
                                        })
                                    })
                                })
                            })
                            .unwrap_or_else(|| "hello".to_string());

                        let model = body
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or("test-model");

                        let events = match Self::pop_agent_turn(&agent_turns, &request) {
                            Some(text) => sse::responses_api_events_exact(&text, model),
                            None => match &*mode.read().unwrap() {
                                ResponseMode::Echo => {
                                    sse::responses_api_events(&format!("Echo: {user_msg}"), model)
                                }
                                ResponseMode::Fixed(text) => {
                                    sse::responses_api_events_exact(text, model)
                                }
                            },
                        };
                        let gate = overrides.fallback_terminal_wait(&request);
                        let stream = paced_events(events, *delay.read().unwrap(), gate);
                        Sse::new(stream)
                            .keep_alive(KeepAlive::default())
                            .into_response()
                    }
                }),
            )
            .route(
                "/v1/messages",
                post(move |headers: HeaderMap, Json(body): Json<Value>| {
                    let log = log_msg.clone();
                    let mode = mode_msg.clone();
                    let overrides = overrides_msg.clone();
                    let agent_turns = agent_turns_msg.clone();
                    let stop_reason = messages_stop_reason.clone();
                    let delay = delay_msg.clone();
                    async move {
                        let auth = Self::extract_auth(&headers);
                        log.record(
                            "POST",
                            "/v1/messages",
                            Some(&body),
                            auth.as_deref(),
                            Self::headers_vec(&headers),
                        );

                        let request =
                            overrides.classify(InferenceEndpoint::Messages, &headers, &body);
                        let chunk_delay = *delay.read().unwrap();
                        if let Some(response) = overrides
                            .response_override(&request, &headers, chunk_delay)
                            .await
                        {
                            return response;
                        }

                        // Anthropic content is either a plain string or an
                        // array of typed blocks; extract the last user text.
                        let user_msg = body
                            .get("messages")
                            .and_then(|m| m.as_array())
                            .and_then(|msgs| {
                                msgs.iter()
                                    .rev()
                                    .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
                            })
                            .and_then(|m| m.get("content"))
                            .and_then(|c| {
                                c.as_str().map(String::from).or_else(|| {
                                    c.as_array().and_then(|blocks| {
                                        blocks.iter().find_map(|b| {
                                            if b.get("type").and_then(Value::as_str) == Some("text")
                                            {
                                                b.get("text")
                                                    .and_then(Value::as_str)
                                                    .map(String::from)
                                            } else {
                                                None
                                            }
                                        })
                                    })
                                })
                            })
                            .unwrap_or_else(|| "hello".to_string());

                        let model = body
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or("test-model");

                        let stop = stop_reason.read().unwrap().clone();
                        let events = match Self::pop_agent_turn(&agent_turns, &request) {
                            Some(text) => sse::messages_api_events(&text, model, &stop),
                            None => match &*mode.read().unwrap() {
                                ResponseMode::Echo => sse::messages_api_events(
                                    &format!("Echo: {user_msg}"),
                                    model,
                                    &stop,
                                ),
                                ResponseMode::Fixed(text) => {
                                    sse::messages_api_events(text, model, &stop)
                                }
                            },
                        };
                        let gate = overrides.fallback_terminal_wait(&request);
                        let stream = paced_events(events, *delay.read().unwrap(), gate);
                        Sse::new(stream)
                            .keep_alive(KeepAlive::default())
                            .into_response()
                    }
                }),
            )
            .route(
                "/v1/models",
                get({
                    let log = log.clone();
                    move || {
                        let log = log.clone();
                        let models = models.clone();
                        let hang = hang_models.clone();
                        async move {
                            log.record("GET", "/v1/models", None, None, Vec::new());
                            if hang.load(std::sync::atomic::Ordering::Acquire) {
                                tokio::time::sleep(Duration::from_secs(3600)).await;
                            }
                            let models_json = models.read().unwrap().clone();
                            Json(json!({
                                "object": "list",
                                "data": models_json,
                            }))
                        }
                    }
                }),
            )
            .route(
                "/v1/settings",
                get({
                    let log = log.clone();
                    let settings = settings.clone();
                    let hang = hang_settings.clone();
                    move || {
                        let log = log.clone();
                        let settings = settings.clone();
                        let overrides = overrides_settings.clone();
                        let hang = hang.clone();
                        async move {
                            log.record("GET", "/v1/settings", None, None, Vec::new());
                            if hang.load(std::sync::atomic::Ordering::Acquire) {
                                tokio::time::sleep(Duration::from_secs(3600)).await;
                            }
                            // Scripts take precedence, so a test can serve a
                            // transient payload before the steady-state value.
                            if let Some(s) = overrides.pop_scripted("/v1/settings") {
                                return s.into_response_paced(None, None).await;
                            }
                            let maybe = settings.read().unwrap().clone();
                            match maybe {
                                Some(s) => Json(s).into_response(),
                                None => StatusCode::NOT_FOUND.into_response(),
                            }
                        }
                    }
                }),
            )
            .route(
                "/v1/privacy/coding-data-retention",
                put({
                    let log = log.clone();
                    move |headers: HeaderMap, Json(body): Json<Value>| {
                        let log = log.clone();
                        async move {
                            let auth = Self::extract_auth(&headers);
                            log.record(
                                "PUT",
                                "/v1/privacy/coding-data-retention",
                                Some(&body),
                                auth.as_deref(),
                                Self::headers_vec(&headers),
                            );
                            // Echo the received flag back like the real
                            // cli-chat-proxy does on success.
                            let opt_out = body
                                .get("codingDataRetentionOptOut")
                                .cloned()
                                .unwrap_or(Value::Bool(false));
                            Json(json!({ "codingDataRetentionOptOut": opt_out }))
                        }
                    }
                }),
            )
            .route(
                "/v1/user",
                get(
                    move |axum::extract::RawQuery(query): axum::extract::RawQuery| {
                        let log = log.clone();
                        let user_tier = user_tier.clone();
                        async move {
                            // Log the query string so a test can count
                            // `?include=subscription` on its own.
                            let path = match query {
                                Some(q) if !q.is_empty() => format!("/v1/user?{q}"),
                                _ => "/v1/user".to_owned(),
                            };
                            log.record("GET", &path, None, None, Vec::new());
                            let tier = user_tier.read().unwrap().clone();
                            let mut body = json!({
                                "userId": "mock-user",
                                "email": "mock-user@test.invalid",
                            });
                            if let Some(t) = tier {
                                body["subscriptionTier"] = json!(t);
                            }
                            Json(body).into_response()
                        }
                    },
                ),
            )
            .route(
                "/sessions/{id}/data",
                post({
                    let log = log_session_data;
                    let overrides = overrides_session_data;
                    move |axum::extract::Path(id): axum::extract::Path<String>,
                          headers: HeaderMap,
                          Json(body): Json<Value>| {
                        let log = log.clone();
                        let overrides = overrides.clone();
                        async move {
                            if let Some(reject) = overrides.auth_rejection(&headers) {
                                return reject;
                            }
                            let auth = Self::extract_auth(&headers);
                            log.record(
                                "POST",
                                &format!("/sessions/{id}/data"),
                                Some(&body),
                                auth.as_deref(),
                                Self::headers_vec(&headers),
                            );
                            StatusCode::OK.into_response()
                        }
                    }
                }),
            )
            .route(
                "/sessions/{id}",
                put({
                    let log = log_session_upsert;
                    let overrides = overrides_session_upsert;
                    move |axum::extract::Path(id): axum::extract::Path<String>,
                          headers: HeaderMap,
                          Json(body): Json<Value>| {
                        let log = log.clone();
                        let overrides = overrides.clone();
                        async move {
                            if let Some(reject) = overrides.auth_rejection(&headers) {
                                return reject;
                            }
                            let auth = Self::extract_auth(&headers);
                            log.record(
                                "PUT",
                                &format!("/sessions/{id}"),
                                Some(&body),
                                auth.as_deref(),
                                Self::headers_vec(&headers),
                            );
                            StatusCode::OK.into_response()
                        }
                    }
                }),
            )
            .route(
                "/v1/storage",
                post({
                    let storage = storage.clone();
                    move |headers: HeaderMap, body: axum::body::Bytes| {
                        let storage = storage.clone();
                        async move { Self::storage_upload_handler(&storage, &headers, &body) }
                    }
                }),
            )
            // 404 reads as an old proxy, so the shell falls back to a plain
            // `POST /v1/storage`.
            .route(
                "/v1/storage/exists",
                get(|| async { StatusCode::NOT_FOUND }),
            )
            .route(
                "/v1/storage/batch_exists",
                post(|| async { StatusCode::NOT_FOUND }),
            )
            .route(
                "/v1/storage/batch_upload_json",
                post(|| async { StatusCode::NOT_FOUND }),
            )
            .route(
                "/v1/storage/batch_upload",
                post(|| async { StatusCode::NOT_FOUND }),
            )
            .route(
                "/v1/storage/limits",
                get(|| async { StatusCode::NOT_FOUND }),
            )
            // Body limit: repo-context archives can exceed axum's 2 MB default.
            .layer(axum::extract::DefaultBodyLimit::max(256 * 1024 * 1024))
    }
}

impl Drop for MockInferenceServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

#[allow(clippy::disallowed_methods)] // test clients hit localhost mocks
#[cfg(test)]
mod tests {
    use super::*;

    const MERMAID_TEXT: &str =
        "Here is a flow:\n\n```mermaid\nflowchart TD\n  A --> B\n```\n\nDone.\n";

    /// Payloads of all `data:` lines in an SSE body, minus the `[DONE]` marker.
    fn sse_data_payloads(body: &str) -> Vec<String> {
        body.lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(|d| d.trim_start().to_owned())
            .filter(|d| d != "[DONE]")
            .collect()
    }

    /// Concatenation of all chat-completion content deltas in an SSE body.
    fn chat_stream_text(body: &str) -> String {
        sse_data_payloads(body)
            .iter()
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .filter_map(|v| {
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .collect()
    }

    /// Concatenation of all responses-API output_text deltas in an SSE body.
    fn responses_stream_text(body: &str) -> String {
        sse_data_payloads(body)
            .iter()
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .filter(|v| v.get("type").and_then(Value::as_str) == Some("response.output_text.delta"))
            .filter_map(|v| v.get("delta").and_then(Value::as_str).map(String::from))
            .collect()
    }

    /// Concatenation of all Anthropic Messages text deltas in an SSE body.
    fn messages_stream_text(body: &str) -> String {
        sse_data_payloads(body)
            .iter()
            .filter_map(|d| serde_json::from_str::<Value>(d).ok())
            .filter(|v| v.get("type").and_then(Value::as_str) == Some("content_block_delta"))
            .filter_map(|v| {
                v.get("delta")
                    .and_then(|d| d.get("text"))
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .collect()
    }

    fn foreground_body(endpoint: InferenceEndpoint, content: &str) -> Value {
        let tools = json!([
            { "type": "function", "function": { "name": "read_file" } },
            { "type": "function", "function": { "name": "write" } }
        ]);
        match endpoint {
            InferenceEndpoint::ChatCompletions | InferenceEndpoint::Messages => json!({
                "model": "test-model",
                "messages": [{ "role": "user", "content": content }],
                "tools": tools,
            }),
            InferenceEndpoint::Responses => json!({
                "model": "test-model",
                "input": [{ "role": "user", "content": content }],
                "tools": tools,
            }),
        }
    }

    fn endpoint_url(server: &MockInferenceServer, endpoint: InferenceEndpoint) -> String {
        let suffix = match endpoint {
            InferenceEndpoint::ChatCompletions => "chat/completions",
            InferenceEndpoint::Responses => "responses",
            InferenceEndpoint::Messages => "messages",
        };
        format!("{}/{suffix}", server.url())
    }

    async fn post_chat(server: &MockInferenceServer, content: &str) -> reqwest::Response {
        reqwest::Client::new()
            .post(format!("{}/chat/completions", server.url()))
            .json(&json!({
                "model": "test-model",
                "messages": [{ "role": "user", "content": content }]
            }))
            .send()
            .await
            .expect("POST /v1/chat/completions")
    }

    async fn post_foreground(
        server: &MockInferenceServer,
        endpoint: InferenceEndpoint,
        request_id: &str,
        content: &str,
    ) -> reqwest::Response {
        reqwest::Client::new()
            .post(endpoint_url(server, endpoint))
            .header("x-echo-agent-req-id", request_id)
            .header("x-echo-agent-turn-idx", "1")
            .json(&foreground_body(endpoint, content))
            .send()
            .await
            .expect("POST foreground inference request")
    }

    async fn read_foreground(
        server: &MockInferenceServer,
        endpoint: InferenceEndpoint,
        request_id: &str,
        content: &str,
    ) -> (reqwest::StatusCode, String) {
        let response = post_foreground(server, endpoint, request_id, content).await;
        let status = response.status();
        let body = response
            .text()
            .await
            .expect("read foreground response body");
        (status, body)
    }

    async fn read_foreground_body(
        server: &MockInferenceServer,
        endpoint: InferenceEndpoint,
        request_id: &str,
        body: Value,
    ) -> (reqwest::StatusCode, String) {
        let response = reqwest::Client::new()
            .post(endpoint_url(server, endpoint))
            .header("x-echo-agent-req-id", request_id)
            .header("x-echo-agent-turn-idx", "1")
            .json(&body)
            .send()
            .await
            .expect("POST foreground inference request");
        let status = response.status();
        let body = response.text().await.expect("read inference response body");
        (status, body)
    }

    #[test]
    fn explicit_request_headers_override_tool_count_heuristic() {
        let overrides = InferenceOverrides::new(None);
        let body = foreground_body(InferenceEndpoint::ChatCompletions, "title");

        let mut headers = HeaderMap::new();
        headers.insert("x-echo-agent-req-id", "title-request".parse().unwrap());
        assert!(
            !overrides
                .classify(InferenceEndpoint::ChatCompletions, &headers, &body)
                .is_foreground()
        );

        headers.insert("x-echo-agent-turn-idx", "1".parse().unwrap());
        assert!(
            overrides
                .classify(InferenceEndpoint::ChatCompletions, &headers, &body)
                .is_foreground()
        );

        headers.insert("x-echo-agent-req-id", "".parse().unwrap());
        headers.insert("x-echo-agent-turn-idx", "".parse().unwrap());
        assert!(
            overrides
                .classify(InferenceEndpoint::ChatCompletions, &headers, &body)
                .is_foreground()
        );
    }

    #[tokio::test]
    async fn dropping_entries_keeps_the_count_exact() {
        let server = MockInferenceServer::start().await.unwrap();
        post_chat(&server, "kept").await;
        let counted = server.request_count();
        let kept = server.requests().len();

        server.set_keep_requests(false);
        post_chat(&server, "dropped").await;

        assert_eq!(server.request_count(), counted + 1);
        let entries = server.requests();
        assert_eq!(entries.len(), kept);
        assert!(
            format!("{:?}", entries.last().unwrap().body).contains("kept"),
            "the surviving entry should be the one recorded before the switch"
        );
    }

    #[tokio::test]
    async fn auxiliary_request_does_not_consume_foreground_expectation() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut expected = server.expect_response(
            "foreground turn",
            InferenceRequestMatcher::foreground(InferenceEndpoint::ChatCompletions),
            ScriptedResponse::text(209, "foreground"),
        );

        let aux = post_chat(&server, "generate a title").await;
        assert_eq!(aux.status(), 200);
        assert!(!expected.is_satisfied());

        let (status, body) = read_foreground(
            &server,
            InferenceEndpoint::ChatCompletions,
            "turn-1",
            "run the task",
        )
        .await;
        assert_eq!(status.as_u16(), 209);
        assert_eq!(body, "foreground");
        expected.wait_received().await;
        expected.wait_satisfied().await;
    }

    #[tokio::test]
    async fn concurrent_matching_requests_claim_each_expectation_once() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut first = server.expect_response(
            "first concurrent turn",
            InferenceRequestMatcher::foreground(InferenceEndpoint::ChatCompletions),
            ScriptedResponse::text(210, "first"),
        );
        let mut second = server.expect_response(
            "second concurrent turn",
            InferenceRequestMatcher::foreground(InferenceEndpoint::ChatCompletions),
            ScriptedResponse::text(211, "second"),
        );

        let (left, right) = tokio::join!(
            read_foreground(
                &server,
                InferenceEndpoint::ChatCompletions,
                "concurrent-left",
                "left",
            ),
            read_foreground(
                &server,
                InferenceEndpoint::ChatCompletions,
                "concurrent-right",
                "right",
            )
        );
        let mut responses = vec![(left.0.as_u16(), left.1), (right.0.as_u16(), right.1)];
        responses.sort_unstable();
        assert_eq!(
            responses,
            vec![(210, "first".to_owned()), (211, "second".to_owned())]
        );
        first.wait_satisfied().await;
        second.wait_satisfied().await;
    }

    #[tokio::test]
    async fn blocked_expectation_reports_lifecycle_and_drop_releases() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut expected = server.expect_response_blocked(
            "blocked foreground turn",
            InferenceRequestMatcher::foreground(InferenceEndpoint::ChatCompletions),
            ScriptedResponse::sse(vec![
                SseEvent::data(r#"{"chunk":1}"#),
                SseEvent::data("done"),
            ]),
        );
        let request = read_foreground(
            &server,
            InferenceEndpoint::ChatCompletions,
            "blocked-turn",
            "block me",
        );
        tokio::pin!(request);

        tokio::select! {
            response = &mut request => panic!("blocked expectation completed early: {:?}", response.0),
            _ = expected.wait_blocked() => {}
        }
        assert!(!expected.is_satisfied());
        let diagnostic = expected.diagnostic();
        assert!(diagnostic.contains("blocked foreground turn"));
        assert!(diagnostic.contains("Blocked"));
        drop(expected);
        let (status, _) = tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .expect("dropping handle releases blocked response");
        assert_eq!(status.as_u16(), 200);
    }

    #[tokio::test]
    async fn concurrent_retry_obeys_same_barrier_and_primary_owns_satisfaction() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut expected = server.expect_response_blocked(
            "blocked retry",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::sse(vec![SseEvent::data("chunk"), SseEvent::data("terminal")]),
        );
        let first = read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "same-call",
            "same body",
        );
        tokio::pin!(first);
        tokio::select! {
            response = &mut first => panic!("primary completed before barrier: {:?}", response.0),
            _ = expected.wait_blocked() => {}
        }

        let retry = read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "same-call",
            "same body",
        );
        tokio::pin!(retry);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut retry)
                .await
                .is_err(),
            "retry bypassed the shared release barrier"
        );
        assert!(!expected.is_satisfied());

        expected.release();
        let ((first_status, _), (retry_status, _)) = tokio::join!(first, retry);
        assert_eq!(first_status.as_u16(), 200);
        assert_eq!(retry_status.as_u16(), 200);
        expected.wait_satisfied().await;
    }

    #[tokio::test]
    async fn release_only_signals_and_late_blocked_waiters_still_succeed() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut expected = server.expect_response_blocked(
            "release ownership",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::json(200, json!({ "ok": true })),
        );
        let request = read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "release-ownership",
            "hello",
        );
        tokio::pin!(request);
        tokio::select! {
            response = &mut request => panic!("response completed before barrier: {:?}", response.0),
            _ = expected.wait_blocked() => {}
        }

        expected.release();
        assert!(!expected.is_satisfied());
        let (status, _) = request.await;
        assert_eq!(status.as_u16(), 200);
        expected.wait_satisfied().await;
        expected.wait_blocked().await;
    }

    #[tokio::test]
    async fn overlapping_duplicate_replays_but_sequential_identical_request_claims_next() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut first = server.expect_response_blocked(
            "overlapping call",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::sse(vec![SseEvent::data("first"), SseEvent::data("terminal")]),
        );
        let mut second = server.expect_response(
            "later identical call",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::text(215, "second expectation"),
        );

        let primary = read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "turn-id",
            "same body",
        );
        tokio::pin!(primary);
        tokio::select! {
            response = &mut primary => panic!("primary completed before barrier: {:?}", response.0),
            _ = first.wait_blocked() => {}
        }
        let replay = tokio::spawn({
            let url = endpoint_url(&server, InferenceEndpoint::Responses);
            let body = foreground_body(InferenceEndpoint::Responses, "same body");
            async move {
                let response = reqwest::Client::new()
                    .post(url)
                    .header("x-echo-agent-req-id", "turn-id")
                    .header("x-echo-agent-turn-idx", "1")
                    .json(&body)
                    .send()
                    .await
                    .expect("POST overlapping duplicate");
                let status = response.status();
                let body = response.text().await.expect("read overlapping duplicate");
                (status, body)
            }
        });
        first.wait_claims(2).await;
        assert!(
            !replay.is_finished(),
            "overlapping duplicate bypassed shared barrier"
        );
        first.release();
        let (primary_result, replay_result) = tokio::join!(primary, replay);
        let (primary_status, _) = primary_result;
        let (replay_status, _) = replay_result.expect("replay task");
        assert_eq!(primary_status.as_u16(), 200);
        assert_eq!(replay_status.as_u16(), 200);
        first.wait_satisfied().await;

        let (status, body) = read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "turn-id",
            "same body",
        )
        .await;
        assert_eq!(status.as_u16(), 215);
        assert_eq!(body, "second expectation");
        second.wait_satisfied().await;
    }

    #[tokio::test]
    async fn changed_body_followup_claims_next_expectation() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut first = server.expect_response(
            "tool call",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::text(214, "tool-call-script"),
        );
        let mut followup = server.expect_response(
            "tool follow-up",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::text(215, "follow-up-script"),
        );
        let first_body = foreground_body(InferenceEndpoint::Responses, "run tool");
        let (status, body) =
            read_foreground_body(&server, InferenceEndpoint::Responses, "turn-id", first_body)
                .await;
        assert_eq!(status.as_u16(), 214);
        assert_eq!(body, "tool-call-script");
        first.wait_satisfied().await;

        let mut followup_body = foreground_body(InferenceEndpoint::Responses, "run tool");
        followup_body["input"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "type": "function_call_output", "call_id": "call_1", "output": "done" }));
        let (status, body) = read_foreground_body(
            &server,
            InferenceEndpoint::Responses,
            "turn-id",
            followup_body,
        )
        .await;
        assert_eq!(status.as_u16(), 215);
        assert_eq!(body, "follow-up-script");
        followup.wait_satisfied().await;
    }

    #[tokio::test]
    async fn cancelling_primary_cleans_up_without_satisfying_or_replaying() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut cancelled = server.expect_response_blocked(
            "cancel primary",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::sse(vec![SseEvent::data("chunk"), SseEvent::data("terminal")]),
        );
        let mut next = server.expect_response(
            "after cancellation",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::text(216, "next expectation"),
        );
        let request = Box::pin(read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "cancel-primary",
            "same body",
        ));
        let mut request = request;
        tokio::select! {
            response = &mut request => panic!("primary completed before cancellation: {:?}", response.0),
            _ = cancelled.wait_blocked() => {}
        }
        drop(request);
        assert!(!cancelled.is_satisfied());

        let (status, body) = read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "cancel-primary",
            "same body",
        )
        .await;
        assert_eq!(status.as_u16(), 216);
        assert_eq!(body, "next expectation");
        next.wait_satisfied().await;
    }

    #[tokio::test]
    async fn cancelling_replay_waits_for_primary_before_satisfaction() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut expected = server.expect_response_blocked(
            "cancel replay",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::sse(vec![SseEvent::data("chunk"), SseEvent::data("terminal")]),
        );
        let primary = read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "cancel-replay",
            "same body",
        );
        tokio::pin!(primary);
        tokio::select! {
            response = &mut primary => panic!("primary completed before barrier: {:?}", response.0),
            _ = expected.wait_blocked() => {}
        }

        let replay = tokio::spawn({
            let url = endpoint_url(&server, InferenceEndpoint::Responses);
            let body = foreground_body(InferenceEndpoint::Responses, "same body");
            async move {
                reqwest::Client::new()
                    .post(url)
                    .header("x-echo-agent-req-id", "cancel-replay")
                    .header("x-echo-agent-turn-idx", "1")
                    .json(&body)
                    .send()
                    .await
                    .expect("POST replay cancellation")
                    .text()
                    .await
                    .expect("read replay cancellation")
            }
        });
        expected.wait_claims(2).await;
        assert!(!replay.is_finished(), "replay bypassed shared barrier");
        replay.abort();
        let _ = replay.await;
        assert!(!expected.is_satisfied());

        expected.release();
        let (status, _) = primary.await;
        assert_eq!(status.as_u16(), 200);
        expected.wait_satisfied().await;
    }

    #[tokio::test]
    #[should_panic(expected = "duplicate inference expectation name `duplicate`")]
    async fn duplicate_expectation_names_are_rejected() {
        let server = MockInferenceServer::start().await.unwrap();
        let _first = server.expect_response(
            "duplicate",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::text(200, "first"),
        );
        let _second = server.expect_response(
            "duplicate",
            InferenceRequestMatcher::auxiliary(InferenceEndpoint::Responses),
            ScriptedResponse::text(200, "second"),
        );
    }

    #[tokio::test]
    async fn unsatisfied_expectation_diagnostic_includes_name_and_state() {
        let server = MockInferenceServer::start().await.unwrap();
        let expected = server.expect_response(
            "must receive a foreground turn",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::text(200, "unused"),
        );
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            expected.assert_satisfied();
        }))
        .expect_err("unsatisfied expectation must panic");
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
            .unwrap_or_default();
        assert!(message.contains("must receive a foreground turn"));
        assert!(message.contains("Pending"));
    }

    #[tokio::test]
    async fn echo_mode_echoes_last_user_message() {
        let server = MockInferenceServer::start().await.unwrap();

        let body = post_chat(&server, "ping pong").await.text().await.unwrap();
        assert_eq!(chat_stream_text(&body), "Echo: ping pong");

        // Echo mode keeps its historical whitespace-collapsing semantics.
        let body = post_chat(&server, "a  b\nc").await.text().await.unwrap();
        assert_eq!(chat_stream_text(&body), "Echo: a b c");
    }

    #[tokio::test]
    async fn fixed_mode_reconstructs_byte_exact_over_http() {
        let server = MockInferenceServer::start().await.unwrap();
        server.set_response(MERMAID_TEXT);

        let body = post_chat(&server, "ignored").await.text().await.unwrap();
        assert_eq!(chat_stream_text(&body), MERMAID_TEXT);

        let body = reqwest::Client::new()
            .post(format!("{}/responses", server.url()))
            .json(&json!({
                "model": "test-model",
                "input": [{ "role": "user", "content": "ignored" }]
            }))
            .send()
            .await
            .expect("POST /v1/responses")
            .text()
            .await
            .unwrap();
        assert_eq!(responses_stream_text(&body), MERMAID_TEXT);

        let body = reqwest::Client::new()
            .post(format!("{}/messages", server.url()))
            .json(&json!({
                "model": "test-model",
                "messages": [{ "role": "user", "content": "ignored" }]
            }))
            .send()
            .await
            .expect("POST /v1/messages")
            .text()
            .await
            .unwrap();
        assert_eq!(messages_stream_text(&body), MERMAID_TEXT);
    }

    #[tokio::test]
    async fn settings_404_until_set_then_200() {
        let server = MockInferenceServer::start().await.unwrap();
        let url = format!("{}/settings", server.url());

        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 404);

        server.set_settings(json!({ "tips": ["t1"] }));
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body, json!({ "tips": ["t1"] }));

        server.preset_allow_access();
        let resp = reqwest::get(&url).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body, json!({ "allow_access": true }));
    }

    #[tokio::test]
    async fn privacy_coding_data_retention_echoes_flag_and_logs() {
        let server = MockInferenceServer::start().await.unwrap();
        let url = format!("{}/privacy/coding-data-retention", server.url());

        for flag in [false, true] {
            let resp = reqwest::Client::new()
                .put(&url)
                .json(&json!({ "codingDataRetentionOptOut": flag }))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), 200);
            let body: Value = resp.json().await.unwrap();
            assert_eq!(body, json!({ "codingDataRetentionOptOut": flag }));
        }

        let entries = server.requests();
        let puts: Vec<_> = entries
            .iter()
            .filter(|e| e.method == "PUT" && e.path == "/v1/privacy/coding-data-retention")
            .collect();
        assert_eq!(puts.len(), 2);
        assert_eq!(
            puts[0].body,
            Some(json!({ "codingDataRetentionOptOut": false }))
        );
        assert_eq!(
            puts[1].body,
            Some(json!({ "codingDataRetentionOptOut": true }))
        );
    }

    #[tokio::test]
    async fn request_bodies_returns_bodies_in_arrival_order() {
        let server = MockInferenceServer::start().await.unwrap();

        post_chat(&server, "first").await.text().await.unwrap();
        // Body-less request in between must be skipped, not break ordering.
        reqwest::get(format!("{}/models", server.url()))
            .await
            .unwrap();
        post_chat(&server, "second").await.text().await.unwrap();

        let bodies = server.request_bodies();
        assert_eq!(bodies.len(), 2);
        assert_eq!(
            bodies[0]["messages"][0]["content"],
            json!("first"),
            "bodies must be in arrival order"
        );
        assert_eq!(bodies[1]["messages"][0]["content"], json!("second"));
    }

    #[tokio::test]
    async fn scripted_responses_serve_fifo_per_path_then_fall_back() {
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response(
            "/v1/chat/completions",
            ScriptedResponse::text(401, "Unauthorized"),
        );
        server.enqueue_response(
            "/v1/chat/completions",
            ScriptedResponse::json(500, json!({ "error": { "message": "boom" } })),
        );

        // FIFO: first the 401 text, then the 500 json.
        let resp = post_chat(&server, "hi").await;
        assert_eq!(resp.status(), 401);
        assert_eq!(resp.text().await.unwrap(), "Unauthorized");

        let resp = post_chat(&server, "hi").await;
        assert_eq!(resp.status(), 500);
        let body: Value = resp.json().await.unwrap();
        assert_eq!(body, json!({ "error": { "message": "boom" } }));

        // Queue drained: falls back to the active mode (echo).
        let body = post_chat(&server, "ping pong").await.text().await.unwrap();
        assert_eq!(chat_stream_text(&body), "Echo: ping pong");

        // Queues are per path: an unrelated endpoint is unaffected.
        server.enqueue_response("/v1/chat/completions", ScriptedResponse::text(503, "later"));
        let resp = reqwest::Client::new()
            .post(format!("{}/responses", server.url()))
            .json(&json!({
                "model": "test-model",
                "input": [{ "role": "user", "content": "hi there" }]
            }))
            .send()
            .await
            .expect("POST /v1/responses");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            responses_stream_text(&resp.text().await.unwrap()),
            "Echo: hi there "
        );
    }

    /// Pins the documented precedence: a script bypasses the required-auth
    /// gate; once the queue empties, the gate is back.
    #[tokio::test]
    async fn scripted_response_takes_precedence_over_required_auth() {
        let server = MockInferenceServer::start_with_required_auth(
            vec![MockModelEntry::new("test-model")],
            "secret-token",
        )
        .await
        .unwrap();
        server.enqueue_response(
            "/v1/chat/completions",
            ScriptedResponse::text(200, "scripted"),
        );

        // No token: the script still serves.
        let resp = post_chat(&server, "hi").await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "scripted");

        // Queue drained: the auth gate applies again.
        let resp = post_chat(&server, "hi").await;
        assert_eq!(resp.status(), 401);
    }

    /// Scripted response headers reach the client (the phase-2 script format's
    /// named consumer: 429 + Retry-After error injection).
    #[tokio::test]
    async fn scripted_response_headers_reach_the_client() {
        let server = MockInferenceServer::start().await.unwrap();
        let mut rate_limited = ScriptedResponse::text(429, "slow down");
        rate_limited
            .headers
            .push(("retry-after".to_string(), "7".to_string()));
        server.enqueue_response("/v1/chat/completions", rate_limited);

        let resp = post_chat(&server, "hi").await;
        assert_eq!(resp.status(), 429);
        assert_eq!(
            resp.headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok()),
            Some("7")
        );
        assert_eq!(resp.text().await.unwrap(), "slow down");
    }

    #[tokio::test]
    async fn scripted_raw_body_served_byte_exact() {
        let server = MockInferenceServer::start().await.unwrap();
        let raw = "data: {\"choices\":[]}\n\ndata: not-json-at-all\n\ndata: [DONE]\n\n";
        server.enqueue_response("/v1/chat/completions", ScriptedResponse::text(200, raw));

        let resp = post_chat(&server, "hi").await;
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), raw);
    }

    #[tokio::test]
    async fn scripted_sse_preserves_event_names_and_order() {
        let server = MockInferenceServer::start().await.unwrap();
        server.enqueue_response(
            "/v1/chat/completions",
            ScriptedResponse::sse(vec![
                SseEvent::with_event("custom.kind", "{\"a\":1}"),
                SseEvent::data("{\"b\":2}"),
            ]),
        );

        let body = post_chat(&server, "hi").await.text().await.unwrap();
        let named_then_plain = body
            .find("event: custom.kind")
            .zip(body.find("data: {\"b\":2}"))
            .is_some_and(|(named, plain)| named < plain);
        assert!(
            body.contains("event: custom.kind") && body.contains("data: {\"a\":1}"),
            "named event must carry both fields, got:\n{body}"
        );
        assert!(named_then_plain, "events must be served in order:\n{body}");
    }

    #[tokio::test]
    async fn matched_barriers_cover_all_endpoints_and_body_modes() {
        for endpoint in [
            InferenceEndpoint::ChatCompletions,
            InferenceEndpoint::Responses,
            InferenceEndpoint::Messages,
        ] {
            for (label, response) in [
                (
                    "sse",
                    ScriptedResponse::sse(vec![
                        SseEvent::data("chunk"),
                        SseEvent::data("terminal"),
                    ]),
                ),
                ("empty-sse", ScriptedResponse::sse(Vec::new())),
                ("json", ScriptedResponse::json(200, json!({ "ok": true }))),
                ("raw", ScriptedResponse::text(200, "raw body")),
            ] {
                let server = MockInferenceServer::start().await.unwrap();
                let mut expected = server.expect_response_blocked(
                    format!("blocked {endpoint:?} {label}"),
                    InferenceRequestMatcher::foreground(endpoint),
                    response,
                );
                let request = read_foreground(&server, endpoint, "body-gate", "hello");
                tokio::pin!(request);
                tokio::select! {
                    response = &mut request => panic!("{endpoint:?}/{label} completed before release: {:?}", response.0),
                    _ = expected.wait_blocked() => {}
                }
                expected.release();
                let (status, _) = tokio::time::timeout(Duration::from_secs(1), request)
                    .await
                    .unwrap_or_else(|_| {
                        panic!("{endpoint:?}/{label} did not complete after release")
                    });
                assert_eq!(status.as_u16(), 200);
                expected.wait_satisfied().await;
            }
        }
    }

    #[tokio::test]
    async fn matched_expectation_precedes_auth_then_auth_resumes() {
        let server = MockInferenceServer::start_with_required_auth(
            vec![MockModelEntry::new("test-model")],
            "secret-token",
        )
        .await
        .unwrap();
        let mut expected = server.expect_response(
            "auth bypass",
            InferenceRequestMatcher::foreground(InferenceEndpoint::Responses),
            ScriptedResponse::text(218, "matched without auth"),
        );

        let response = post_foreground(
            &server,
            InferenceEndpoint::Responses,
            "auth-bypass",
            "first",
        )
        .await;
        assert_eq!(response.status().as_u16(), 218);
        assert_eq!(response.text().await.unwrap(), "matched without auth");
        expected.wait_satisfied().await;

        let response = post_foreground(
            &server,
            InferenceEndpoint::Responses,
            "auth-fallback",
            "second",
        )
        .await;
        assert_eq!(response.status(), 401);
    }

    #[tokio::test]
    async fn compatibility_completion_gate_covers_fallback_and_scripted_sse() {
        for endpoint in [
            InferenceEndpoint::ChatCompletions,
            InferenceEndpoint::Responses,
            InferenceEndpoint::Messages,
        ] {
            let server = MockInferenceServer::start().await.unwrap();
            server.hold_agent_completions();
            server.set_agent_turns([format!("{endpoint:?} turn")]);
            let request = read_foreground(&server, endpoint, "global-gate", "hello");
            tokio::pin!(request);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut request)
                    .await
                    .is_err(),
                "{endpoint:?} bypassed the compatibility gate"
            );
            server.release_agent_completions();
            tokio::time::timeout(Duration::from_secs(1), request)
                .await
                .unwrap_or_else(|_| panic!("{endpoint:?} did not complete after release"));
        }

        let server = MockInferenceServer::start().await.unwrap();
        server.hold_agent_completions();
        server.enqueue_response(
            "/v1/responses",
            ScriptedResponse::sse(vec![SseEvent::data("chunk"), SseEvent::data("terminal")]),
        );
        let request = read_foreground(
            &server,
            InferenceEndpoint::Responses,
            "scripted-global-gate",
            "hello",
        );
        tokio::pin!(request);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut request)
                .await
                .is_err(),
            "scripted SSE bypassed the compatibility gate"
        );
        server.release_agent_completions();
        tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .expect("scripted SSE completes after release");
    }

    #[tokio::test]
    async fn compatibility_completion_gate_does_not_hold_json_or_raw() {
        for response in [
            ScriptedResponse::json(219, json!({ "ok": true })),
            ScriptedResponse::text(220, "raw body"),
        ] {
            let server = MockInferenceServer::start().await.unwrap();
            server.hold_agent_completions();
            server.enqueue_response("/v1/responses", response);
            let (status, _) = tokio::time::timeout(
                Duration::from_secs(1),
                read_foreground(
                    &server,
                    InferenceEndpoint::Responses,
                    "compat-non-sse",
                    "hello",
                ),
            )
            .await
            .expect("compatibility JSON/raw must not wait for the SSE gate");
            assert!(matches!(status.as_u16(), 219 | 220));
        }
    }

    #[tokio::test]
    async fn request_log_captures_arbitrary_headers() {
        let server = MockInferenceServer::start().await.unwrap();

        reqwest::Client::new()
            .post(format!("{}/chat/completions", server.url()))
            .header("authorization", "Bearer log-me")
            .header("x-test-marker", "zap")
            .json(&json!({
                "model": "test-model",
                "messages": [{ "role": "user", "content": "hi" }]
            }))
            .send()
            .await
            .expect("POST /v1/chat/completions");

        let entry = server.requests().pop().expect("one logged request");
        assert_eq!(entry.header("x-test-marker"), Some("zap"));
        assert_eq!(entry.header("X-Test-Marker"), Some("zap"));
        assert_eq!(entry.header("authorization"), Some("Bearer log-me"));
        assert_eq!(entry.authorization.as_deref(), Some("Bearer log-me"));
        assert_eq!(entry.header("x-absent"), None);
    }

    #[tokio::test]
    async fn required_auth_enforced_in_both_response_modes() {
        let server = MockInferenceServer::start_with_required_auth(
            vec![MockModelEntry::new("test-model")],
            "secret-token",
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let url = format!("{}/chat/completions", server.url());
        let req_body = json!({
            "model": "test-model",
            "messages": [{ "role": "user", "content": "hi there" }]
        });

        // Echo mode: missing auth rejected, valid auth streams the echo.
        let resp = client.post(&url).json(&req_body).send().await.unwrap();
        assert_eq!(resp.status(), 401);
        let resp = client
            .post(&url)
            .header("authorization", "Bearer secret-token")
            .json(&req_body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(
            chat_stream_text(&resp.text().await.unwrap()),
            "Echo: hi there"
        );

        // Fixed mode: same auth gate, fixed text streamed byte-exact.
        server.set_response(MERMAID_TEXT);
        let resp = client.post(&url).json(&req_body).send().await.unwrap();
        assert_eq!(resp.status(), 401);
        let resp = client
            .post(&url)
            .header("authorization", "Bearer secret-token")
            .json(&req_body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(chat_stream_text(&resp.text().await.unwrap()), MERMAID_TEXT);
    }

    #[tokio::test]
    async fn session_writeback_routes_are_logged() {
        let server = MockInferenceServer::start().await.unwrap();
        let client = reqwest::Client::new();
        let origin = server.origin();

        let data = client
            .post(format!("{origin}/sessions/abc/data"))
            .json(&json!({
                "messages": [{ "content": "x" }],
                "metadata": { "title": "Manual", "cwd": "/" }
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(data.status(), 200);

        let upsert = client
            .put(format!("{origin}/sessions/abc"))
            .json(&json!({ "session": { "title": "Manual" }, "agentId": "a" }))
            .send()
            .await
            .unwrap();
        assert_eq!(upsert.status(), 200);

        let reqs = server.requests();
        let data_req = reqs
            .iter()
            .find(|r| r.method == "POST" && r.path == "/sessions/abc/data")
            .expect("POST /sessions/abc/data");
        assert_eq!(
            data_req
                .body
                .as_ref()
                .and_then(|b| b.get("metadata"))
                .and_then(|m| m.get("title"))
                .and_then(|t| t.as_str()),
            Some("Manual")
        );
        assert!(
            reqs.iter()
                .any(|r| r.method == "PUT" && r.path == "/sessions/abc"),
            "PUT /sessions/abc must be logged"
        );
    }

    #[tokio::test]
    async fn session_writeback_respects_required_auth() {
        let server = MockInferenceServer::start_with_required_auth(
            vec![MockModelEntry::new("test-model")],
            "secret-token",
        )
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let url = format!("{}/sessions/abc/data", server.origin());
        let body = json!({ "messages": [], "metadata": { "title": "T", "cwd": "/" } });

        let denied = client.post(&url).json(&body).send().await.unwrap();
        assert_eq!(denied.status(), 401);

        let ok = client
            .post(&url)
            .header("authorization", "Bearer secret-token")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
    }
}
