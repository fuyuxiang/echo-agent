//! Layer 3: Content controller.
//!
//! An idle pager only renders a splash screen — not useful for scroll,
//! stream, or resize scenarios. [`ContentController`] wraps the shared
//! [`MockInferenceServer`] from `echo-agent-test-support` and provides the
//! env vars that point the bundled shell agent at it, so the pager ends
//! up rendering real agent output.
//!
//! The caller controls the response text via [`ContentController::set_response`].
//! The mock server streams the set response to every inference request.

use std::path::Path;

use anyhow::{Context, Result};
use echo_agent_test_support::{MockInferenceServer, TestSandbox};

pub use echo_agent_test_support::mock_server::LogEntry;
pub use echo_agent_test_support::mock_server::MockModelEntry as MockModel;
pub use echo_agent_test_support::mock_server::StorageUpload;
pub use echo_agent_test_support::sse;
pub use echo_agent_test_support::{
    InferenceEndpoint, InferenceExpectation, InferenceRequestMatcher, ScriptedResponse, SseEvent,
};

/// The endpoint-specific expectations for one logical foreground agent turn.
#[must_use = "keep the handle to synchronize or verify the logical agent turn"]
pub struct AgentTurnExpectation {
    expectations: [InferenceExpectation; 2],
}

impl AgentTurnExpectation {
    /// Wait until either supported pager backend claims the turn.
    pub async fn wait_received(&mut self) {
        let [responses, chat_completions] = &mut self.expectations;
        tokio::select! {
            _ = responses.wait_received() => {}
            _ = chat_completions.wait_received() => {}
        }
    }

    /// Wait until the active backend reaches this turn's terminal barrier.
    pub async fn wait_blocked(&mut self) {
        let [responses, chat_completions] = &mut self.expectations;
        tokio::select! {
            _ = responses.wait_blocked() => {}
            _ = chat_completions.wait_blocked() => {}
        }
    }

    /// Release both endpoint variants of this turn's terminal barrier.
    pub fn release(&self) {
        for expectation in &self.expectations {
            expectation.release();
        }
    }

    /// Wait until the active backend completes this turn.
    pub async fn wait_satisfied(&mut self) {
        let [responses, chat_completions] = &mut self.expectations;
        tokio::select! {
            _ = responses.wait_satisfied() => {}
            _ = chat_completions.wait_satisfied() => {}
        }
    }

    /// Whether either supported endpoint completed this logical turn.
    pub fn is_satisfied(&self) -> bool {
        self.expectations
            .iter()
            .any(InferenceExpectation::is_satisfied)
    }

    /// Panic unless one supported backend completed this turn.
    pub fn assert_satisfied(&self) {
        assert!(
            self.is_satisfied(),
            "logical agent turn was not satisfied: {}",
            self.diagnostic(),
        );
    }

    /// Describe both endpoint variants for aggregated failure output.
    pub fn diagnostic(&self) -> String {
        format!(
            "{}; {}",
            self.expectations[0].diagnostic(),
            self.expectations[1].diagnostic(),
        )
    }

    /// Endpoint expectations that were not claimed by the active backend.
    pub fn unsatisfied_diagnostics(&self) -> impl Iterator<Item = String> + '_ {
        self.expectations
            .iter()
            .filter(|expectation| !expectation.is_satisfied())
            .map(InferenceExpectation::diagnostic)
    }
}

/// Drives content into the pager by serving a mock inference endpoint that
/// the bundled shell agent hits for `/v1/chat/completions` and `/v1/responses`.
///
/// Thin wrapper over the shared [`MockInferenceServer`]: adds the isolated
/// `$HOME` sandbox and pager env plumbing, and applies the harness defaults
/// the pager depends on (always-200 `/v1/settings`, fixed default response).
///
/// Shuts the server down on drop (the inner server's `Drop`).
pub struct ContentController {
    server: MockInferenceServer,
    sandbox: TestSandbox,
}

impl ContentController {
    /// Start the mock inference server on a random local port.
    ///
    /// Must be called from within a tokio runtime.
    pub async fn start() -> Result<Self> {
        Self::start_with_models(vec![MockModel::new("test-model")]).await
    }

    /// Start the mock server with a custom set of models returned by
    /// `GET /v1/models`. Use [`MockModel::with_agent_type`] to configure
    /// models with different harness types for agent-type-mismatch tests.
    pub async fn start_with_models(models: Vec<MockModel>) -> Result<Self> {
        let server = MockInferenceServer::start_with_models(models)
            .await
            .context("start mock inference server")?;
        // Pre-delegation parity, both load-bearing for PTY tests: settings
        // must be 200 `{"allow_access": true}` (the shared 404-until-set
        // default strands the pager on the upsell screen), and the response
        // mode must be a fixed text (the shared default is echo).
        server.preset_allow_access();
        server.set_response(default_response_text());

        let mut sandbox = TestSandbox::builder().mock_url(server.url()).build();
        // Keep unrelated autocomplete work out of PTY timing assertions.
        sandbox.set_env("ECHO_AGENT_PROMPT_SUGGESTIONS", "false");

        Ok(Self { server, sandbox })
    }

    /// Base URL of the mock server, e.g. `http://127.0.0.1:41823/v1`.
    pub fn url(&self) -> String {
        self.server.url()
    }

    /// Isolated `$HOME` directory that the pager should use (keeps its ~/.echo-agent
    /// cache/state out of the real home during tests).
    pub fn home(&self) -> &Path {
        self.sandbox.home()
    }

    /// Filesystem and environment used by content-backed spawns.
    pub fn sandbox(&self) -> &TestSandbox {
        &self.sandbox
    }

    /// Replace the mocked assistant response. All subsequent chat completion
    /// requests will stream this text word-by-word.
    pub fn set_response(&self, text: impl Into<String>) {
        self.server.set_response(text);
    }

    /// Queue a compatibility response for the next request on `path`.
    /// Inference callers should use a matched expectation; this remains for
    /// non-inference one-shots such as `"/v1/settings"`.
    pub fn enqueue_response(&self, path: impl Into<String>, response: ScriptedResponse) {
        self.server.enqueue_response(path, response);
    }

    /// Access the underlying mock inference server for advanced scripting.
    pub fn server(&self) -> &MockInferenceServer {
        &self.server
    }

    /// Pace the mocked SSE streams: each event is emitted after `delay`.
    /// `None` restores instant streaming. Use to hold a turn visibly
    /// "streaming" long enough to interact with it (e.g. Esc-cancel tests).
    pub fn set_chunk_delay(&self, delay: Option<std::time::Duration>) {
        self.server.set_chunk_delay(delay);
    }

    /// Register a named response for the next matching inference request.
    pub fn expect_response(
        &self,
        name: impl Into<String>,
        matcher: InferenceRequestMatcher,
        response: ScriptedResponse,
    ) -> InferenceExpectation {
        self.server.expect_response(name, matcher, response)
    }

    /// Register one named response held immediately before its terminal event.
    pub fn expect_response_blocked(
        &self,
        name: impl Into<String>,
        matcher: InferenceRequestMatcher,
        response: ScriptedResponse,
    ) -> InferenceExpectation {
        self.server.expect_response_blocked(name, matcher, response)
    }

    /// Register the same named foreground text turn for both pager backends.
    pub fn expect_agent_turn(
        &self,
        name: impl AsRef<str>,
        text: impl AsRef<str>,
    ) -> AgentTurnExpectation {
        self.expect_agent_turn_with_responses(
            name,
            ScriptedResponse::sse(sse::responses_api_script_exact(text.as_ref(), "test-model")),
            ScriptedResponse::sse(sse::chat_completion_script_exact(
                text.as_ref(),
                "test-model",
            )),
        )
    }

    /// Register the same named foreground text turn for both pager backends,
    /// blocked immediately before its terminal event.
    pub fn expect_agent_turn_blocked(
        &self,
        name: impl AsRef<str>,
        text: impl AsRef<str>,
    ) -> AgentTurnExpectation {
        self.expect_agent_turn_with_responses_inner(
            name.as_ref(),
            ScriptedResponse::sse(sse::responses_api_script_exact(text.as_ref(), "test-model")),
            ScriptedResponse::sse(sse::chat_completion_script_exact(
                text.as_ref(),
                "test-model",
            )),
            true,
        )
    }

    /// Register endpoint-specific responses for one logical foreground turn.
    pub fn expect_agent_turn_with_responses(
        &self,
        name: impl AsRef<str>,
        responses: ScriptedResponse,
        chat_completions: ScriptedResponse,
    ) -> AgentTurnExpectation {
        self.expect_agent_turn_with_responses_inner(
            name.as_ref(),
            responses,
            chat_completions,
            false,
        )
    }

    fn expect_agent_turn_with_responses_inner(
        &self,
        name: &str,
        responses: ScriptedResponse,
        chat_completions: ScriptedResponse,
        blocked: bool,
    ) -> AgentTurnExpectation {
        let register = |endpoint, suffix: &str, response| {
            let name = format!("{name} ({suffix})");
            let matcher = InferenceRequestMatcher::foreground(endpoint);
            if blocked {
                self.expect_response_blocked(name, matcher, response)
            } else {
                self.expect_response(name, matcher, response)
            }
        };
        AgentTurnExpectation {
            expectations: [
                register(InferenceEndpoint::Responses, "responses", responses),
                register(
                    InferenceEndpoint::ChatCompletions,
                    "chat completions",
                    chat_completions,
                ),
            ],
        }
    }

    /// Number of inference requests the pager has made so far.
    pub fn request_count(&self) -> u32 {
        self.server.request_count()
    }

    /// Whether the server has seen a chat completion request.
    pub fn has_chat_completion(&self) -> bool {
        self.server.has_chat_completion_request() || self.server.has_responses_request()
    }

    /// Snapshot of all received requests — useful for test diagnostics.
    pub fn requests(&self) -> Vec<LogEntry> {
        self.server.requests()
    }

    pub fn request_bodies(&self) -> Vec<serde_json::Value> {
        self.server.request_bodies()
    }

    // ── Mock storage controls (park-on-401 e2e) ────────────────────────────

    /// Flip the mock `/v1/storage` 401 gate (the auth-outage window).
    pub fn set_storage_unauthorized(&self, unauthorized: bool) {
        self.server.set_storage_unauthorized(unauthorized);
    }

    /// Total `/v1/storage` upload attempts, including 401-rejected ones.
    pub fn storage_request_count(&self) -> u32 {
        self.server.storage_request_count()
    }

    /// Snapshot of accepted (HTTP 200) `/v1/storage` uploads.
    pub fn storage_uploads(&self) -> Vec<StorageUpload> {
        self.server.storage_uploads()
    }
}

fn default_response_text() -> String {
    "Hello from the pty_harness mock inference server.".to_owned()
}

#[allow(clippy::disallowed_methods)] // test clients hit localhost mocks
#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

    fn foreground_request(endpoint: InferenceEndpoint) -> (&'static str, serde_json::Value) {
        match endpoint {
            InferenceEndpoint::ChatCompletions => (
                "/chat/completions",
                serde_json::json!({
                    "model": "test-model",
                    "messages": [{ "role": "user", "content": "hello" }]
                }),
            ),
            InferenceEndpoint::Responses => (
                "/responses",
                serde_json::json!({
                    "model": "test-model",
                    "input": [{ "role": "user", "content": "hello" }]
                }),
            ),
            InferenceEndpoint::Messages => unreachable!("pager helper supports two backends"),
        }
    }

    async fn read_foreground(url: String, endpoint: InferenceEndpoint) -> String {
        let (path, body) = foreground_request(endpoint);
        reqwest::Client::new()
            .post(format!("{url}{path}"))
            .header("x-echo-agent-turn-idx", "1")
            .header("x-echo-agent-req-id", format!("direct-{endpoint:?}"))
            .json(&body)
            .send()
            .await
            .expect("send direct foreground request")
            .text()
            .await
            .expect("read direct foreground response")
    }

    /// The pre-delegation mock always served 200 `{"allow_access": true}`;
    /// the shared server defaults to 404-until-set. A 404 strands the pager
    /// on the EchoAgent upsell screen and breaks every PTY test.
    #[tokio::test]
    async fn settings_endpoint_allows_access_by_default() {
        let content = ContentController::start().await.unwrap();

        let resp = reqwest::get(format!("{}/settings", content.url()))
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body, serde_json::json!({ "allow_access": true }));
    }

    /// The pre-delegation mock streamed a fixed default text to every
    /// request; the shared server defaults to echo.
    #[tokio::test]
    async fn default_response_streams_fixed_text() {
        let content = ContentController::start().await.unwrap();

        let body = reqwest::Client::new()
            .post(format!("{}/chat/completions", content.url()))
            .json(&serde_json::json!({
                "model": "test-model",
                "messages": [{ "role": "user", "content": "anything" }]
            }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();

        let streamed: String = body
            .lines()
            .filter_map(|l| l.strip_prefix("data:"))
            .map(str::trim_start)
            .filter(|d| *d != "[DONE]")
            .filter_map(|d| serde_json::from_str::<serde_json::Value>(d).ok())
            .filter_map(|v| {
                v.get("choices")
                    .and_then(|c| c.get(0))
                    .and_then(|c| c.get("delta"))
                    .and_then(|d| d.get("content"))
                    .and_then(serde_json::Value::as_str)
                    .map(String::from)
            })
            .collect();
        assert_eq!(streamed, default_response_text());
        assert!(content.has_chat_completion());
    }

    #[tokio::test]
    async fn logical_turn_accepts_either_supported_endpoint() {
        for endpoint in [
            InferenceEndpoint::ChatCompletions,
            InferenceEndpoint::Responses,
        ] {
            let content = ContentController::start().await.unwrap();
            let mut turn = content.expect_agent_turn("either endpoint", "MATCHED_TURN");
            let request = tokio::spawn(read_foreground(content.url(), endpoint));

            tokio::time::timeout(EXPECTATION_TIMEOUT, turn.wait_received())
                .await
                .expect("logical turn received through active endpoint");
            let body = tokio::time::timeout(EXPECTATION_TIMEOUT, request)
                .await
                .expect("active endpoint response completed")
                .expect("direct request task completed");
            tokio::time::timeout(EXPECTATION_TIMEOUT, turn.wait_satisfied())
                .await
                .expect("logical turn satisfied through active endpoint");

            assert!(body.contains("MATCHED_TURN"), "body: {body}");
            assert!(turn.is_satisfied());
            turn.assert_satisfied();
            let diagnostics: Vec<_> = turn.unsatisfied_diagnostics().collect();
            assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
            let unused = match endpoint {
                InferenceEndpoint::Responses => "chat completions",
                InferenceEndpoint::ChatCompletions => "responses",
                InferenceEndpoint::Messages => unreachable!(),
            };
            assert!(diagnostics[0].contains(unused), "{diagnostics:#?}");
            assert!(diagnostics[0].contains("Pending"), "{diagnostics:#?}");
        }
    }

    #[tokio::test]
    async fn logical_blocked_turn_observes_release_and_satisfaction() {
        for endpoint in [
            InferenceEndpoint::ChatCompletions,
            InferenceEndpoint::Responses,
        ] {
            let content = ContentController::start().await.unwrap();
            let mut turn =
                content.expect_agent_turn_blocked("blocked turn", "BLOCKED_MATCHED_TURN");
            let mut request = tokio::spawn(read_foreground(content.url(), endpoint));

            tokio::time::timeout(EXPECTATION_TIMEOUT, turn.wait_received())
                .await
                .expect("logical blocked turn received");
            tokio::time::timeout(EXPECTATION_TIMEOUT, turn.wait_blocked())
                .await
                .expect("logical blocked turn reached terminal barrier");
            assert!(!turn.is_satisfied());
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut request)
                    .await
                    .is_err(),
                "response completed before release"
            );

            turn.release();
            let body = tokio::time::timeout(EXPECTATION_TIMEOUT, request)
                .await
                .expect("response completed after release")
                .expect("direct request task completed");
            tokio::time::timeout(EXPECTATION_TIMEOUT, turn.wait_satisfied())
                .await
                .expect("logical blocked turn satisfied after release");

            assert!(body.contains("BLOCKED_MATCHED_TURN"), "body: {body}");
            turn.assert_satisfied();
        }
    }

    #[tokio::test]
    async fn unused_logical_turn_reports_both_endpoint_variants_and_fails_contract() {
        let content = ContentController::start().await.unwrap();
        let turn = content.expect_agent_turn("unused logical turn", "unused");

        assert!(!turn.is_satisfied());
        let diagnostics: Vec<_> = turn.unsatisfied_diagnostics().collect();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
        assert!(
            diagnostics.iter().any(|d| d.contains("responses")),
            "{diagnostics:#?}"
        );
        assert!(
            diagnostics.iter().any(|d| d.contains("chat completions")),
            "{diagnostics:#?}"
        );
        assert!(diagnostics.iter().all(|d| d.contains("Pending")));
        let aggregate = turn.diagnostic();
        assert!(aggregate.contains("responses"), "{aggregate}");
        assert!(aggregate.contains("chat completions"), "{aggregate}");

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            turn.assert_satisfied();
        }));
        assert!(
            panic.is_err(),
            "unused logical turn must fail one-of-two contract"
        );
    }
}
