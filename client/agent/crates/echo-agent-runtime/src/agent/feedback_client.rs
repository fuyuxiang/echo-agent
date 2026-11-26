//! REST client for feedback collection via cli-chat-proxy.
//!
//! This client handles:
//! - Syncing session signals to cli-chat-proxy
//! - Submitting feedback responses
//! - Completing/dismissing feedback requests
//! - Creating feedback requests (when triggered by heuristics)

use std::sync::Arc;

use anyhow::{Context, Result};
use reqwest::RequestBuilder;
use serde::de::DeserializeOwned;

// Import feedback wire types from cli-chat-proxy
use echo_agent_chat_proxy_types::feedback_types::{
    ClientType, CreateFeedbackRequestInput, CreateFeedbackRequestResponse,
    FeedbackHeuristicsConfig, FeedbackRequestUpdateResponse, FeedbackResponse, FeedbackSubmission,
    SessionEventRequest, SessionEventResponse, SessionSignalsUpdate, SessionSignalsUpdateResponse,
};

/// Client version header sent on every request to cli-chat-proxy for version gating.
const CLIENT_VERSION_HEADER: &str = "x-echo-agent-client-version";

// ============================================================================
// Turn delta wire types (local to echo-agent-runtime until cli-chat-proxy catches up)
// ============================================================================

/// Per-turn delta sent at the end of every turn via
/// `POST /v1/sessions/{session_id}/turn-deltas`.
///
/// Each field falls into one of four categories:
///
/// - **Delta** — the *change* since the previous turn end (computed as
///   `current_cumulative - previous_turn_snapshot`). For the first turn,
///   the previous snapshot is zero.
/// - **Turn-level** — an absolute value measured only for *this* turn,
///   reset between turns. `None` when the event did not occur this turn.
/// - **Accumulated** — a cumulative total since session start, monotonically
///   increasing across turns.
/// - **Context** — session/turn metadata that is neither a counter nor a
///   measurement (e.g. IDs, timestamps, client type).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionTurnDelta {
    // ── Context fields ──────────────────────────────────────────────────
    /// **[context]** Which client surface produced this record (e.g. CLI, TUI).
    pub client_type: ClientType,

    /// **[context]** 1-based turn number at the time of this snapshot. Equals
    /// the cumulative `turn_count` from `SessionSignals`.
    pub turn_number: i64,

    // ── Delta counters ──────────────────────────────────────────────────
    // Each is `current_cumulative - previous_turn_snapshot`.
    /// **[delta]** Number of tool calls made during this turn.
    pub delta_tool_calls: i64,

    /// **[delta]** Number of tool calls that failed during this turn.
    pub delta_tool_failures: i64,

    /// **[delta]** Number of errors (including sampling errors) during this turn.
    pub delta_errors: i64,

    /// **[delta]** Number of user cancellations (Ctrl+C) during this turn.
    pub delta_cancellations: i64,

    /// **[delta]** Number of regeneration requests during this turn.
    pub delta_regenerations: i64,

    /// **[delta]** Number of conversation compactions during this turn.
    pub delta_compactions: i64,

    /// **[delta]** Number of edit-and-retry actions (user rewinds prompt)
    /// during this turn.
    pub delta_edit_and_retries: i64,

    /// **[delta]** Number of positive ratings (thumbs-up) during this turn.
    pub delta_positive_ratings: i64,

    /// **[delta]** Number of negative ratings (thumbs-down) during this turn.
    pub delta_negative_ratings: i64,

    /// **[delta]** Number of assistant messages produced during this turn
    /// (may be >1 when tool-call rounds generate intermediate messages).
    pub delta_assistant_messages: i64,

    /// **[delta]** Number of long idle pauses (>60 s) that occurred during
    /// this turn.
    pub delta_long_pauses: i64,

    /// **[delta]** Number of successful tool uses during this turn. Derived
    /// as `delta_tool_calls − delta_tool_failures`.
    pub delta_successful_tool_uses: i64,

    // ── Turn-level snapshot values ──────────────────────────────────────
    /// **[turn-level]** Consecutive cancellation streak at turn end. This is
    /// a point-in-time snapshot (not a diff) — it resets to 0 when a turn
    /// completes normally.
    pub consecutive_cancellations: i64,

    // ── Turn-level latency ──────────────────────────────────────────────
    // Absolute measurements for this turn's inference request only.
    // `None` when no inference occurred during the turn.
    /// **[turn-level]** Time-to-first-token for this turn's model response
    /// (milliseconds). `None` when no inference occurred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<i64>,

    /// **[turn-level]** Total wall-clock response time for this turn's model
    /// response (milliseconds). `None` when no inference occurred.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_response_time_ms: Option<i64>,

    /// **[turn-level]** Inter-token latency p50 for this turn (ms).
    /// Computed from the token intervals collected during this turn only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_p50_ms: Option<i64>,

    /// **[turn-level]** Inter-token latency p99 for this turn (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_p99_ms: Option<i64>,

    /// **[turn-level]** Inter-token latency maximum for this turn (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_max_ms: Option<i64>,

    /// **[turn-level]** Inter-token latency mean for this turn (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub itl_mean_ms: Option<i64>,

    // ── Accumulated / snapshot session-level values ─────────────────────
    /// **[accumulated]** Current context window usage as a percentage (0–100)
    /// at turn end. Read from cumulative `SessionSignals.context_window_usage`.
    pub context_window_usage: i64,

    /// **[accumulated]** Primary model ID (most recently used model). Read
    /// from cumulative `SessionSignals.primary_model_id`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,

    // ── Turn-level outcome / served checkpoint ──────────────────────────
    /// Whole-turn wall-clock duration (prompt→final response), ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_duration_ms: Option<i64>,

    /// Terminal outcome: `"completed"` | `"cancelled"` | `"error"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_outcome: Option<String>,

    /// Served model fingerprint (upstream `system_fingerprint`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,

    // ── Turn-level tool / error detail ──────────────────────────────────
    /// **[turn-level]** Distinct tool names invoked during this turn
    /// (deduplicated, sorted, capped at 100 entries). Reset each turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools_used_this_turn: Vec<String>,

    /// **[turn-level]** Error type strings that occurred during this turn
    /// (e.g. `"timeout"`, `"rate_limit"`, `"tool_error"`). Reset each turn.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_types_this_turn: Vec<String>,

    /// **[turn-level]** Per-tool success/failure breakdown for this turn,
    /// JSON-serialized array of `{ tool_name, successes, failures }`.
    /// Empty string when no tool calls occurred. Reset each turn.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool_outcomes: String,

    // ── Accumulated totals ──────────────────────────────────────────────
    /// **[accumulated]** Total tool calls since session start.
    /// Read from cumulative `SessionSignals.tool_call_count`.
    pub cumulative_tool_calls: i64,

    /// **[accumulated]** Total errors since session start.
    /// Read from cumulative `SessionSignals.error_count`.
    pub cumulative_errors: i64,

    /// **[accumulated]** Wall-clock seconds elapsed since session start.
    /// Read from cumulative `SessionSignals.session_duration_seconds`.
    pub session_duration_seconds: i64,

    /// **[accumulated]** Sum of token counts across all compactions since
    /// session start. Read from `SessionSignals.total_tokens_before_compaction`.
    #[serde(default)]
    pub total_tokens_before_compaction: i64,

    /// **[context]** Arbitrary JSON metadata blob.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,

    /// **[context]** Prompt/request ID that initiated this turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,

    /// **[context]** Wall-clock time when the session was created. Used for
    /// BQ partitioning on the backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_start_at: Option<chrono::DateTime<chrono::Utc>>,

    // ── Feedback state ──────────────────────────────────────────────────
    /// **[accumulated]** Total number of feedback requests sent this session.
    /// Supplied by `FeedbackHeuristics`, not the signals actor.
    #[serde(default)]
    pub feedback_requests_sent: i64,

    /// **[accumulated]** Wall-clock timestamp of the most recent feedback
    /// request sent this session. Supplied by `FeedbackHeuristics`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_feedback_request_at: Option<chrono::DateTime<chrono::Utc>>,

    // ── Turn-level token counts ─────────────────────────────────────────
    /// **[turn-level]** Number of response (completion minus reasoning)
    /// tokens generated during this turn. `None` when no inference occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_tokens: Option<i64>,

    /// **[turn-level]** Number of thinking/reasoning tokens generated during
    /// this turn. `None` when no inference occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_tokens: Option<i64>,

    // ── LOC Attribution Deltas ──────────────────────────────────────────
    // Each is `current_cumulative - previous_turn_snapshot`, same as the
    // counter deltas above. Tracks lines-of-code changes attributed to
    // the agent vs. the human during this turn.
    /// **[delta]** Lines added by the agent during this turn.
    #[serde(default)]
    pub delta_agent_lines_added: i64,

    /// **[delta]** Lines removed by the agent during this turn.
    #[serde(default)]
    pub delta_agent_lines_removed: i64,

    /// **[delta]** Agent-added lines that were reverted during this turn.
    #[serde(default)]
    pub delta_agent_lines_added_reverted: i64,

    /// **[delta]** Agent-removed lines that were reverted during this turn.
    #[serde(default)]
    pub delta_agent_lines_removed_reverted: i64,

    /// **[delta]** Lines added by the human during this turn.
    #[serde(default)]
    pub delta_human_lines_added: i64,

    /// **[delta]** Lines removed by the human during this turn.
    #[serde(default)]
    pub delta_human_lines_removed: i64,

    /// **[delta]** Human-added lines that were reverted during this turn.
    #[serde(default)]
    pub delta_human_lines_added_reverted: i64,

    /// **[delta]** Human-removed lines that were reverted during this turn.
    #[serde(default)]
    pub delta_human_lines_removed_reverted: i64,

    /// **[delta]** New distinct files touched by the agent during this turn.
    #[serde(default)]
    pub delta_agent_files_touched: i64,

    /// **[delta]** New distinct files touched by the human during this turn.
    #[serde(default)]
    pub delta_human_files_touched: i64,

    /// **[delta]** New distinct files touched (union of agent + human)
    /// during this turn.
    #[serde(default)]
    pub delta_total_files_touched: i64,

    /// **[context]** Whether LOC (lines-of-code) attribution tracking was
    /// enabled for this session.  When `false`, all `delta_*` LOC fields
    /// above are meaningless zeros — the hunk tracker was never spawned.
    /// When `true`, zeros mean "tracking was active but no code changed."
    /// Defaults to `false` for backwards-compat with old clients that
    /// don't send this field.
    #[serde(default)]
    pub loc_tracking_enabled: bool,
}

/// Response from the turn-deltas endpoint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionTurnDeltaResponse {
    pub session_id: String,
    pub turn_number: i64,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}

/// HTTP error from the feedback/signals API with a preserved status code.
///
/// Used to let callers distinguish auth failures (401) from transient errors
/// without fragile string matching on error messages.
#[derive(Debug, thiserror::Error)]
#[error("{context} failed with status {status}: {body}")]
pub(crate) struct FeedbackApiError {
    pub status: reqwest::StatusCode,
    pub context: &'static str,
    pub body: String,
}

impl FeedbackApiError {
    /// Returns `true` if this is a 401 Unauthorized response.
    pub(crate) fn is_unauthorized(&self) -> bool {
        self.status == reqwest::StatusCode::UNAUTHORIZED
    }

    /// Returns `true` if this is a 403 Forbidden response.
    pub(crate) fn is_forbidden(&self) -> bool {
        self.status == reqwest::StatusCode::FORBIDDEN
    }
}

/// Client for the feedback collection API via cli-chat-proxy.
#[derive(Clone)]
pub struct FeedbackClient {
    http: reqwest::Client,
    client: reqwest_middleware::ClientWithMiddleware,
    base_url: String,
    credentials: crate::util::echo_agent_auth_credentials::EchoAgentAuthCredentials,
    session_id: Option<String>,
}

impl FeedbackClient {
    pub fn new(base_url: impl Into<String>, user_token: Option<String>) -> Self {
        let http = crate::http::shared_client();
        let credentials =
            crate::util::echo_agent_auth_credentials::EchoAgentAuthCredentials::new(user_token);
        let client = Self::build_middleware_client(&http, &credentials);
        Self {
            http,
            client,
            base_url: base_url.into(),
            credentials,
            session_id: None,
        }
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_alpha_test_key(mut self, key: Option<String>) -> Self {
        self.credentials.alpha_test_key = key;
        self.rebuild_middleware();
        self
    }

    pub fn with_deployment_key(mut self, key: Option<String>) -> Self {
        self.credentials.deployment_key = key;
        self.rebuild_middleware();
        self
    }

    /// Create a FeedbackClient with a custom reqwest Client.
    pub fn with_client(
        http: reqwest::Client,
        base_url: impl Into<String>,
        user_token: Option<String>,
    ) -> Self {
        let credentials =
            crate::util::echo_agent_auth_credentials::EchoAgentAuthCredentials::new(user_token);
        let client = Self::build_middleware_client(&http, &credentials);
        Self {
            http,
            client,
            base_url: base_url.into(),
            credentials,
            session_id: None,
        }
    }

    pub(crate) fn with_auth_manager(
        mut self,
        auth_manager: std::sync::Arc<crate::auth::AuthManager>,
    ) -> Self {
        self.credentials = self.credentials.with_auth_manager(auth_manager);
        self.rebuild_middleware();
        self
    }

    /// Whether this client can refresh credentials on a 401: requires both an
    /// attached `AuthManager` and a wired `TokenRefresher` (e.g. static
    /// deployment-key sessions return false).
    pub(crate) fn has_token_refresher(&self) -> bool {
        self.credentials
            .auth_manager()
            .is_some_and(|am| am.has_refresher_attached())
    }

    /// Rebuild the middleware-wrapped client from the current credentials.
    /// Called by each builder method so the middleware sees the final state.
    fn rebuild_middleware(&mut self) {
        self.client = Self::build_middleware_client(&self.http, &self.credentials);
    }

    fn build_middleware_client(
        http: &reqwest::Client,
        credentials: &crate::util::echo_agent_auth_credentials::EchoAgentAuthCredentials,
    ) -> reqwest_middleware::ClientWithMiddleware {
        let provider = Self::make_auth_provider(credentials);
        // max_retries=0: the middleware stamps the auth header but does NOT
        // drive its own ServerRejected recovery on 401.  Background consumers
        // (signals sync, turn deltas) handle retry at the application level
        // via with_one_shot_auth_retry / try_refresh_and_retry_sync, which
        // first wait for the proactive refresh to complete before falling back
        // to active recovery.  This prevents the 401-amplification pattern
        // where the middleware's eager ServerRejected refresh races with every
        // other auth consumer during token-expiry windows.
        reqwest_middleware::ClientBuilder::new(http.clone())
            .with(echo_agent_auth::AuthRetryMiddleware::new(provider, 0))
            .build()
    }

    fn make_auth_provider(
        credentials: &crate::util::echo_agent_auth_credentials::EchoAgentAuthCredentials,
    ) -> Arc<dyn echo_agent_auth::AuthCredentialProvider> {
        if let Some(am) = credentials.auth_manager() {
            Arc::new(
                crate::auth::credential_provider::ShellAuthCredentialProvider::new(
                    am.clone(),
                    credentials.deployment_key.clone(),
                    credentials.alpha_test_key.clone(),
                ),
            )
        } else {
            let wire_bearer = credentials
                .deployment_key
                .clone()
                .or(credentials.user_token.clone());
            Arc::new(echo_agent_auth::StaticAuthCredentialProvider::new(
                Box::new(credentials.clone()),
                wire_bearer,
            ))
        }
    }

    fn record_401_attribution_if_needed(
        &self,
        response: &reqwest::Response,
        stamp: Option<&echo_agent_auth::StampedBearerSuffix>,
        op: &str,
    ) {
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            && let Some(am) = self.credentials.auth_manager()
        {
            // Attribute what the middleware stamped, never a re-resolved or
            // constructor-time credential (see `StampedBearerSuffix`).
            // `None` = the request went out with no bearer.
            crate::auth::attribution::record_consumer_401(
                am.as_ref(),
                self.session_id.as_deref(),
                crate::auth::attribution::ConsumerKind::FeedbackClient,
                op,
                stamp.map(|s| s.0.as_str()),
            );
        }
    }

    pub(crate) async fn try_refresh_credentials(&self) -> bool {
        let Some(manager) = self.credentials.auth_manager() else {
            return false;
        };
        manager
            .try_recover_unauthorized(crate::auth::recovery::RecoverySource::Background)
            .await
    }

    /// Wait for another consumer (proactive refresh, main request path) to
    /// refresh the token.  Returns `true` if the token changed within the
    /// timeout.  Background consumers call this before driving their own
    /// `ServerRejected` recovery to avoid amplifying 401 bursts.
    pub(crate) async fn wait_for_token_refresh(&self, timeout: std::time::Duration) -> bool {
        let Some(manager) = self.credentials.auth_manager() else {
            return false;
        };
        manager.wait_for_token_refresh(timeout).await
    }

    /// `true` iff the attached `AuthManager` has a non-aged-out
    /// permanent-failure verdict from the IdP.
    pub(crate) fn is_auth_permanently_failed(&self) -> bool {
        self.credentials
            .auth_manager()
            .is_some_and(|am| am.has_permanent_failure())
    }

    /// Create a POST request builder with common headers.
    fn post(&self, url: &str) -> RequestBuilder {
        self.add_common_headers(self.http.post(url))
    }

    /// Create a GET request builder with common headers.
    fn get(&self, url: &str) -> RequestBuilder {
        self.add_common_headers(self.http.get(url))
    }

    fn add_common_headers(&self, builder: RequestBuilder) -> RequestBuilder {
        let builder = builder
            .header(CLIENT_VERSION_HEADER, echo_agent_version::VERSION)
            .header(
                crate::http::CLIENT_MODE_HEADER,
                crate::http::process_client_mode(),
            );
        // User-token auth requires the companion marker header for proxy
        // routing. Deployment keys do not need it.
        if self.credentials.deployment_key.is_none() {
            builder.header("X-Echo-Agent-Token-Auth", "echo-agent-cli")
        } else {
            builder
        }
    }

    async fn send_json<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        context: &'static str,
    ) -> Result<T> {
        let request =
            echo_agent_file_utils::trace_context::inject_trace_context_into_request(request);
        let req = request.build().context(context)?;
        let (response, stamp) = echo_agent_auth::execute_with_stamp(&self.client, req)
            .await
            .context(context)?;

        self.record_401_attribution_if_needed(&response, stamp.as_ref(), context);

        if response.status() == reqwest::StatusCode::FORBIDDEN {
            tracing::debug!("{context} rejected (403), skipping");
            anyhow::bail!("{context} rejected (403)");
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(FeedbackApiError {
                status,
                context,
                body,
            }
            .into());
        }

        response
            .json()
            .await
            .with_context(|| format!("Failed to parse {} response", context))
    }

    async fn send_empty(&self, request: RequestBuilder, context: &'static str) -> Result<()> {
        let request =
            echo_agent_file_utils::trace_context::inject_trace_context_into_request(request);
        let req = request.build().context(context)?;
        let (response, stamp) = echo_agent_auth::execute_with_stamp(&self.client, req)
            .await
            .context(context)?;

        self.record_401_attribution_if_needed(&response, stamp.as_ref(), context);

        if response.status() == reqwest::StatusCode::FORBIDDEN {
            tracing::debug!("{context} rejected (403), skipping");
            return Ok(());
        }

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown error".to_string());
            return Err(FeedbackApiError {
                status,
                context,
                body,
            }
            .into());
        }

        Ok(())
    }

    /// Update session signals.
    /// POST /v1/sessions/{session_id}/signals
    pub async fn update_signals(
        &self,
        session_id: &str,
        update: &SessionSignalsUpdate,
    ) -> Result<SessionSignalsUpdateResponse> {
        let url = format!("{}/sessions/{}/signals", self.base_url, session_id);
        let request = self.post(&url).json(update);
        self.send_json(request, "Signals update").await
    }

    /// Record a session event.
    /// POST /v1/sessions/{session_id}/events
    pub async fn record_event(
        &self,
        session_id: &str,
        event: &SessionEventRequest,
    ) -> Result<SessionEventResponse> {
        let url = format!("{}/sessions/{}/events", self.base_url, session_id);
        let request = self.post(&url).json(event);
        self.send_json(request, "Event recording").await
    }

    /// Submit feedback.
    /// POST /v1/feedback
    pub async fn submit_feedback(
        &self,
        submission: &FeedbackSubmission,
    ) -> Result<FeedbackResponse> {
        let url = format!("{}/feedback", self.base_url);
        let request = self.post(&url).json(submission);
        self.send_json(request, "Feedback submission").await
    }

    /// Complete a feedback request.
    /// POST /v1/feedback/requests/{request_id}/complete
    pub async fn complete_request(
        &self,
        request_id: &str,
        submission: &FeedbackSubmission,
    ) -> Result<()> {
        let url = format!(
            "{}/feedback/requests/{}/complete",
            self.base_url, request_id
        );
        let request = self.post(&url).json(submission);
        self.send_empty(request, "Completing feedback request")
            .await
    }

    /// Dismiss a feedback request.
    /// POST /v1/feedback/requests/{request_id}/dismiss
    pub async fn dismiss_request(&self, request_id: &str) -> Result<FeedbackRequestUpdateResponse> {
        let url = format!("{}/feedback/requests/{}/dismiss", self.base_url, request_id);
        let request = self.post(&url);
        self.send_json(request, "Dismissing feedback request").await
    }

    /// Create a new feedback request.
    /// POST /v1/feedback/requests
    ///
    /// Called when the agent decides to request feedback (based on heuristics).
    /// This creates a record in BigQuery before the FeedbackRequest notification
    /// is sent to the client.
    pub async fn create_feedback_request(
        &self,
        input: &CreateFeedbackRequestInput,
    ) -> Result<CreateFeedbackRequestResponse> {
        let url = format!("{}/feedback/requests", self.base_url);
        let request = self.post(&url).json(input);
        self.send_json(request, "Creating feedback request").await
    }

    /// Get the active feedback heuristics configuration.
    /// GET /v1/feedback/config
    ///
    /// This fetches the current feedback configuration from the server,
    /// including tier thresholds, sample rates, and feedback modes.
    pub async fn get_feedback_config(&self) -> Result<FeedbackHeuristicsConfig> {
        let url = format!("{}/feedback/config", self.base_url);
        let request = self.get(&url);
        self.send_json(request, "Fetching feedback config").await
    }

    /// Send a per-turn delta to the backend.
    /// POST /v1/sessions/{session_id}/turn-deltas
    ///
    /// Called at the end of every turn to stream time-series data for
    /// regression tracking and session analytics.
    pub(crate) async fn send_turn_delta(
        &self,
        session_id: &str,
        delta: &SessionTurnDelta,
    ) -> Result<SessionTurnDeltaResponse> {
        let url = format!("{}/sessions/{}/turn-deltas", self.base_url, session_id);
        let request = self.post(&url).json(delta);
        self.send_json(request, "Sending turn delta").await
    }
}

/// Helper to create a SessionSignalsUpdate from local session signals.
pub fn signals_to_update(
    signals: &crate::session::signals::SessionSignals,
    client_type: ClientType,
) -> SessionSignalsUpdate {
    SessionSignalsUpdate {
        client_type,
        total_turns: Some(signals.turn_count as i64),
        user_message_count: Some(signals.user_message_count as i64),
        assistant_message_count: Some(signals.assistant_message_count as i64),
        cancellation_count: Some(signals.cancellation_count as i64),
        consecutive_cancellations: Some(signals.consecutive_cancellations as i64),
        error_count: Some(signals.error_count as i64),
        tool_failure_count: Some(signals.tool_failure_count as i64),
        tool_call_count: Some(signals.tool_call_count as i64),
        compaction_count: Some(signals.compaction_count as i64),
        regeneration_count: Some(signals.regeneration_count as i64),
        edit_and_retry_count: Some(signals.edit_and_retry_count as i64),
        positive_ratings: Some(signals.positive_ratings as i64),
        negative_ratings: Some(signals.negative_ratings as i64),
        long_pauses_count: Some(signals.long_pauses_count as i64),
        session_duration_seconds: Some(signals.session_duration_seconds as i64),
        tools_used: signals.tools_used.clone(),
        models_used: signals.models_used.clone(),
        primary_model_id: signals.primary_model_id.clone(),
        // Latency metrics
        avg_time_to_first_token_ms: Some(signals.avg_time_to_first_token_ms as i64),
        avg_response_time_ms: Some(signals.avg_response_time_ms as i64),
        min_time_to_first_token_ms: Some(signals.min_time_to_first_token_ms as i64),
        max_time_to_first_token_ms: Some(signals.max_time_to_first_token_ms as i64),
        latency_sample_count: Some(signals.latency_sample_count as i64),
        // ITL metrics (session-level aggregates)
        // Guard p50/p99 with itl_sample_count > 0 so that fresh sessions
        // (no ITL measured) send None → SQL NULL, preserving the "not yet
        // reported" semantic in the nullable PG columns.
        last_itl_p50_ms: signals.itl_p50_ms.map(|v| v as i64),
        last_itl_p99_ms: signals.itl_p99_ms.map(|v| v as i64),
        worst_itl_max_ms: signals.itl_max_ms.map(|v| v as i64),
        avg_itl_mean_ms: signals.itl_mean_ms.map(|v| v as i64),
        total_chunk_count: Some(signals.total_chunk_count as i64),
        itl_sample_count: Some(signals.itl_sample_count as i64),
        // Inference idle timeout tracing
        inference_idle_timeouts: Some(signals.inference_idle_timeouts as i64),
        inference_idle_timeout_configured_secs: signals
            .inference_idle_timeout_configured_secs
            .map(|v| v as i64),
        // Legacy client-side doom-loop detection removed; keep its columns null.
        doom_loop_warnings: None,
        doom_loop_terminations: None,
        doom_loop_threshold: None,
        doom_loop_ro_threshold: None,
        // Doom-loop recovery (server-detected, client-resampled) tracing
        doom_loop_recovery_fired: Some(
            signals.doom_loop_recovery_attempts > 0
                || signals.doom_loop_recovery_accepted_after_budget > 0,
        ),
        doom_loop_recovery_attempts: Some(signals.doom_loop_recovery_attempts as i64),
        doom_loop_recovery_accepted_after_budget: Some(
            signals.doom_loop_recovery_accepted_after_budget as i64,
        ),
        doom_loop_recovery_top_trigger: signals.doom_loop_recovery_top_trigger.clone(),
        doom_loop_recovery_aborted_chunks: Some(signals.doom_loop_recovery_aborted_chunks as i64),
        // GCS upload queue tracing
        gcs_queue_enqueued: Some(signals.gcs_queue_enqueued as i64),
        gcs_queue_uploaded: Some(signals.gcs_queue_uploaded as i64),
        gcs_queue_failed: Some(signals.gcs_queue_failed as i64),
        gcs_queue_fallbacks: Some(signals.gcs_queue_fallbacks as i64),
        gcs_queue_circuit_breaker_trips: Some(signals.gcs_queue_circuit_breaker_trips as i64),
        gcs_queue_pending: Some(signals.gcs_queue_pending as i64),
        gcs_queue_pending_bytes: Some(signals.gcs_queue_pending_bytes as i64),
        gcs_queue_orphans_cleaned: Some(signals.gcs_queue_orphans_cleaned as i64),
        // LOC Attribution
        agent_lines_added: Some(signals.agent_lines_added),
        agent_lines_removed: Some(signals.agent_lines_removed),
        agent_lines_added_reverted: Some(signals.agent_lines_added_reverted),
        agent_lines_removed_reverted: Some(signals.agent_lines_removed_reverted),
        human_lines_added: Some(signals.human_lines_added),
        human_lines_removed: Some(signals.human_lines_removed),
        human_lines_added_reverted: Some(signals.human_lines_added_reverted),
        human_lines_removed_reverted: Some(signals.human_lines_removed_reverted),
        agent_files_touched: Some(signals.agent_files_touched as i64),
        human_files_touched: Some(signals.human_files_touched as i64),
        total_files_touched: Some(signals.total_files_touched as i64),
        metadata: None,
    }
}

/// Build a `SessionTurnDelta` from a `TurnDeltaSnapshot` produced by the signals actor.
///
/// `feedback_requests_sent` and `last_feedback_request_at` are supplied by the
/// caller (from `FeedbackHeuristics`) because the signals actor does not track
/// feedback state.
/// `request_id` is the prompt/request identifier for this turn.
/// `loc_tracking_enabled` indicates whether the LOC attribution hunk tracker
/// was active for this session. When `false`, LOC delta fields are zeros
/// because the tracker was never spawned — not because no code changed.
pub(crate) fn snapshot_to_turn_delta(
    snapshot: &crate::session::signals::TurnDeltaSnapshot,
    client_type: ClientType,
    request_id: Option<String>,
    feedback_requests_sent: u32,
    last_feedback_request_at: Option<chrono::DateTime<chrono::Utc>>,
    loc_tracking_enabled: bool,
    turn_duration_ms: Option<i64>,
    turn_outcome: Option<String>,
    model_fingerprint: Option<String>,
) -> SessionTurnDelta {
    let metadata = {
        let mut metadata = serde_json::Map::new();
        if let Some(mode) = snapshot.start_prompt_mode.as_ref() {
            metadata.insert("startPromptMode".to_owned(), serde_json::json!(mode));
        }
        if let Some(mode) = snapshot.end_prompt_mode.as_ref() {
            metadata.insert("endPromptMode".to_owned(), serde_json::json!(mode));
        }
        (!metadata.is_empty()).then_some(serde_json::Value::Object(metadata))
    };
    let d = &snapshot.delta;
    let c = &snapshot.current;
    SessionTurnDelta {
        client_type,
        turn_number: d.turn_number as i64,
        // Deltas
        delta_tool_calls: d.delta_tool_calls,
        delta_tool_failures: d.delta_tool_failures,
        delta_errors: d.delta_errors,
        delta_cancellations: d.delta_cancellations,
        delta_regenerations: d.delta_regenerations,
        delta_compactions: d.delta_compactions,
        delta_edit_and_retries: d.delta_edit_and_retries,
        delta_positive_ratings: d.delta_positive_ratings,
        delta_negative_ratings: d.delta_negative_ratings,
        delta_assistant_messages: d.delta_assistant_messages,
        delta_long_pauses: d.delta_long_pauses,
        delta_successful_tool_uses: d.delta_successful_tool_uses,
        // Turn-level snapshot values
        consecutive_cancellations: d.consecutive_cancellations as i64,
        // Turn-level absolute values
        time_to_first_token_ms: d.last_time_to_first_token_ms.map(|v| v as i64),
        total_response_time_ms: d.last_total_response_time_ms.map(|v| v as i64),
        // Per-turn ITL (delta uses u64, wire type uses i64)
        itl_p50_ms: d.last_itl_p50_ms.map(|v| v as i64),
        itl_p99_ms: d.last_itl_p99_ms.map(|v| v as i64),
        itl_max_ms: d.last_itl_max_ms.map(|v| v as i64),
        itl_mean_ms: d.last_itl_mean_ms.map(|v| v as i64),
        context_window_usage: c.context_window_usage as i64,
        model_id: c.primary_model_id.clone(),
        turn_duration_ms,
        turn_outcome,
        model_fingerprint,
        tools_used_this_turn: d.tools_this_turn.clone(),
        error_types_this_turn: d.error_types_this_turn.clone(),
        tool_outcomes: if d.tool_outcomes_this_turn.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&d.tool_outcomes_this_turn).unwrap_or_default()
        },
        // Cumulative totals
        cumulative_tool_calls: c.tool_call_count as i64,
        cumulative_errors: c.error_count as i64,
        session_duration_seconds: c.session_duration_seconds as i64,
        total_tokens_before_compaction: c.total_tokens_before_compaction as i64,
        metadata,
        request_id,
        session_start_at: None, // set by caller if available
        feedback_requests_sent: feedback_requests_sent as i64,
        last_feedback_request_at,
        response_tokens: d.response_tokens.map(|v| v as i64),
        thinking_tokens: d.thinking_tokens.map(|v| v as i64),
        // LOC Attribution
        delta_agent_lines_added: d.delta_agent_lines_added,
        delta_agent_lines_removed: d.delta_agent_lines_removed,
        delta_agent_lines_added_reverted: d.delta_agent_lines_added_reverted,
        delta_agent_lines_removed_reverted: d.delta_agent_lines_removed_reverted,
        delta_human_lines_added: d.delta_human_lines_added,
        delta_human_lines_removed: d.delta_human_lines_removed,
        delta_human_lines_added_reverted: d.delta_human_lines_added_reverted,
        delta_human_lines_removed_reverted: d.delta_human_lines_removed_reverted,
        delta_agent_files_touched: d.delta_agent_files_touched,
        delta_human_files_touched: d.delta_human_files_touched,
        delta_total_files_touched: d.delta_total_files_touched,
        loc_tracking_enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signals_to_update() {
        let signals = crate::session::signals::SessionSignals {
            turn_count: 10,
            user_message_count: 10,
            assistant_message_count: 10,
            error_count: 1,
            tool_failure_count: 0,
            cancellation_count: 0,
            consecutive_cancellations: 0,
            regeneration_count: 1,
            edit_and_retry_count: 2,
            positive_ratings: 3,
            negative_ratings: 1,
            long_pauses_count: 4,
            has_reverted: false,
            compaction_count: 2,
            total_tokens_before_compaction: 20_000,
            context_window_usage: 50,
            tool_call_count: 5,
            tools_used: vec!["read_file".to_string(), "search_replace".to_string()],
            models_used: vec!["echo-agent-3".to_string()],
            primary_model_id: Some("echo-agent-3".to_string()),
            session_duration_seconds: 120,
            // Latency metrics
            avg_time_to_first_token_ms: 150,
            avg_response_time_ms: 2500,
            min_time_to_first_token_ms: 100,
            max_time_to_first_token_ms: 300,
            latency_sample_count: 5,
            // ITL metrics
            itl_p50_ms: Some(45),
            itl_p99_ms: Some(180),
            itl_max_ms: Some(350),
            itl_mean_ms: Some(62),
            total_chunk_count: 1200,
            itl_sample_count: 15,
            itl_digest: None,
            itl_sum_ms: 0,
            itl_interval_count: 0,
            doom_loop_recovery_attempts: 2,
            doom_loop_recovery_accepted_after_budget: 1,
            doom_loop_recovery_top_trigger: Some("tail_repetition:4@thinking".to_string()),
            doom_loop_recovery_aborted_chunks: 421,
            ..Default::default()
        };

        let update = signals_to_update(&signals, ClientType::Agent);

        assert_eq!(update.total_turns, Some(10));
        assert_eq!(update.error_count, Some(1));
        assert_eq!(update.compaction_count, Some(2));
        assert_eq!(update.cancellation_count, Some(0));
        assert_eq!(update.consecutive_cancellations, Some(0));
        assert_eq!(update.tool_failure_count, Some(0));
        assert_eq!(update.tool_call_count, Some(5));
        assert_eq!(update.session_duration_seconds, Some(120));
        assert_eq!(update.tools_used.len(), 2);
        assert_eq!(update.models_used.len(), 1);
        assert_eq!(update.primary_model_id, Some("echo-agent-3".to_string()));
        // New counter assertions
        assert_eq!(update.edit_and_retry_count, Some(2));
        assert_eq!(update.positive_ratings, Some(3));
        assert_eq!(update.negative_ratings, Some(1));
        assert_eq!(update.long_pauses_count, Some(4));
        // Latency assertions
        assert_eq!(update.avg_time_to_first_token_ms, Some(150));
        assert_eq!(update.avg_response_time_ms, Some(2500));
        assert_eq!(update.min_time_to_first_token_ms, Some(100));
        assert_eq!(update.max_time_to_first_token_ms, Some(300));
        assert_eq!(update.latency_sample_count, Some(5));
        // ITL assertions
        assert_eq!(update.last_itl_p50_ms, Some(45));
        assert_eq!(update.last_itl_p99_ms, Some(180));
        assert_eq!(update.worst_itl_max_ms, Some(350));
        assert_eq!(update.avg_itl_mean_ms, Some(62));
        assert_eq!(update.total_chunk_count, Some(1200));
        assert_eq!(update.itl_sample_count, Some(15));
        // Doom-loop recovery assertions (legacy detection columns stay null)
        assert_eq!(update.doom_loop_recovery_fired, Some(true));
        assert_eq!(update.doom_loop_recovery_attempts, Some(2));
        assert_eq!(update.doom_loop_recovery_accepted_after_budget, Some(1));
        assert_eq!(
            update.doom_loop_recovery_top_trigger.as_deref(),
            Some("tail_repetition:4@thinking")
        );
        assert_eq!(update.doom_loop_recovery_aborted_chunks, Some(421));
        assert_eq!(update.doom_loop_warnings, None);
    }

    #[test]
    fn test_snapshot_to_turn_delta_includes_prompt_mode_metadata() {
        let snapshot = crate::session::signals::TurnDeltaSnapshot {
            current: crate::session::signals::SessionSignals::default(),
            delta: crate::session::signals::SessionSignalsDelta {
                turn_number: 1,
                ..Default::default()
            },
            start_prompt_mode: Some("plan".to_string()),
            end_prompt_mode: Some("agent".to_string()),
            turn_input_tokens: 0,
            turn_output_tokens: 0,
            turn_cached_input_tokens: 0,
        };

        let delta = snapshot_to_turn_delta(
            &snapshot,
            ClientType::Agent,
            Some("request-1".to_string()),
            0,
            None,
            false,
            Some(1500),
            Some("completed".to_string()),
            Some("fp_test_123".to_string()),
        );

        assert_eq!(
            delta.metadata,
            Some(serde_json::json!({
                "startPromptMode": "plan",
                "endPromptMode": "agent"
            }))
        );
        assert_eq!(delta.turn_duration_ms, Some(1500));
        assert_eq!(delta.turn_outcome.as_deref(), Some("completed"));
        assert_eq!(delta.model_fingerprint.as_deref(), Some("fp_test_123"));
    }

    #[test]
    fn test_signals_to_update_fresh_session_itl_none() {
        // When itl_sample_count == 0, p50/p99 must be None (SQL NULL)
        // to preserve the "not yet reported" semantic in the nullable PG columns.
        let signals = crate::session::signals::SessionSignals {
            turn_count: 1,
            user_message_count: 1,
            assistant_message_count: 1,
            error_count: 0,
            tool_failure_count: 0,
            cancellation_count: 0,
            consecutive_cancellations: 0,
            regeneration_count: 0,
            edit_and_retry_count: 0,
            positive_ratings: 0,
            negative_ratings: 0,
            long_pauses_count: 0,
            has_reverted: false,
            compaction_count: 0,
            total_tokens_before_compaction: 0,
            context_window_usage: 0,
            tool_call_count: 0,
            tools_used: vec![],
            models_used: vec![],
            primary_model_id: None,
            session_duration_seconds: 10,
            avg_time_to_first_token_ms: 0,
            avg_response_time_ms: 0,
            min_time_to_first_token_ms: 0,
            max_time_to_first_token_ms: 0,
            latency_sample_count: 0,
            // No ITL measured yet
            itl_p50_ms: None,
            itl_p99_ms: None,
            itl_max_ms: None,
            itl_mean_ms: None,
            total_chunk_count: 0,
            itl_sample_count: 0,
            itl_digest: None,
            itl_sum_ms: 0,
            itl_interval_count: 0,
            ..Default::default()
        };

        let update = signals_to_update(&signals, ClientType::Agent);

        // p50/p99 must be None when no ITL has been measured
        assert_eq!(update.last_itl_p50_ms, None);
        assert_eq!(update.last_itl_p99_ms, None);
        // max and mean are also None when no ITL has been measured
        assert_eq!(update.worst_itl_max_ms, None);
        assert_eq!(update.avg_itl_mean_ms, None);
        assert_eq!(update.total_chunk_count, Some(0));
        assert_eq!(update.itl_sample_count, Some(0));
    }
}

#[allow(clippy::disallowed_methods)] // test clients hit localhost mocks
#[cfg(test)]
mod forbidden_tests {
    use super::*;
    use axum::{Router, response::IntoResponse, routing::post};
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    fn forbidden_handler() -> impl IntoResponse {
        (axum::http::StatusCode::FORBIDDEN, "ZDR team")
    }

    #[tokio::test]
    async fn send_empty_returns_ok_on_403() {
        let router = Router::new().route(
            "/v1/feedback/requests/{id}/complete",
            post(|| async { forbidden_handler() }),
        );
        let (addr, _) = start_server(router).await;

        let client = FeedbackClient::with_client(
            reqwest::Client::new(),
            format!("http://{addr}/v1"),
            Some("tok".into()),
        );
        let submission: FeedbackSubmission = serde_json::from_value(serde_json::json!({
            "sessionId": "s1",
            "clientType": "agent",
            "feedbackType": "rating",
        }))
        .unwrap();
        let result = client.complete_request("req-1", &submission).await;
        assert!(result.is_ok(), "403 on send_empty must return Ok");
    }

    #[tokio::test]
    async fn send_json_bails_on_403_with_clear_message() {
        let router = Router::new().route(
            "/v1/feedback/config",
            axum::routing::get(|| async { forbidden_handler() }),
        );
        let (addr, _) = start_server(router).await;

        let client = FeedbackClient::with_client(
            reqwest::Client::new(),
            format!("http://{addr}/v1"),
            Some("tok".into()),
        );
        let result = client.get_feedback_config().await;
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("(403)"),
            "error must mention 403, got: {err}"
        );
    }
}

/// Auth resolve + 401 recovery tests.
#[cfg(test)]
mod auth_refresh_tests {
    use super::*;
    use crate::auth::{AuthManager, AuthMode, EchoAgentAuth, EchoAgentComConfig};
    use axum::{Router, routing::get};
    use chrono::{Duration, Utc};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tokio::net::TcpListener;

    async fn start_server(router: Router) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (addr, handle)
    }

    /// `send_empty` must sign with the AuthManager bearer. Pre-fix
    /// it skipped the resolve and went out unauthenticated.
    #[tokio::test]
    async fn send_empty_signs_request_with_active_auth() {
        let captured = Arc::new(parking_lot::Mutex::new(None::<String>));
        let captured_for_handler = captured.clone();
        let router = Router::new().route(
            "/v1/feedback/requests/{id}/complete",
            axum::routing::post(move |headers: axum::http::HeaderMap| {
                let captured = captured_for_handler.clone();
                async move {
                    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
                        *captured.lock() = Some(auth.to_str().unwrap_or("").to_owned());
                    }
                    axum::http::StatusCode::OK
                }
            }),
        );
        let (addr, _server) = start_server(router).await;

        let dir = tempfile::tempdir().unwrap();
        let am = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
        am.hot_swap(EchoAgentAuth {
            key: "fresh-from-auth-manager".into(),
            auth_mode: AuthMode::ApiKey,
            create_time: Utc::now(),
            user_id: "user-42".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        });

        let client = FeedbackClient::new(
            format!("http://{addr}/v1"),
            Some("STALE-build-time-token".into()),
        )
        .with_auth_manager(am.clone());

        let submission: FeedbackSubmission = serde_json::from_value(serde_json::json!({
            "sessionId": "s1",
            "clientType": "agent",
            "feedbackType": "rating",
        }))
        .unwrap();
        client
            .complete_request("req-1", &submission)
            .await
            .expect("send_empty must succeed when authenticated");

        let sent = captured.lock().clone().expect("server saw the request");
        assert_eq!(sent, "Bearer fresh-from-auth-manager");
    }

    /// Outgoing bearer must match `AuthManager.current()`, not the
    /// FeedbackClient's build-time snapshot.
    #[tokio::test]
    async fn feedback_client_uses_active_auth_for_each_request() {
        let captured = Arc::new(parking_lot::Mutex::new(None::<String>));
        let captured_for_handler = captured.clone();
        let router = Router::new().route(
            "/v1/feedback/config",
            get(move |headers: axum::http::HeaderMap| {
                let captured = captured_for_handler.clone();
                async move {
                    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
                        *captured.lock() = Some(auth.to_str().unwrap_or("").to_owned());
                    }
                    axum::Json(serde_json::json!({}))
                }
            }),
        );
        let (addr, _server) = start_server(router).await;

        let dir = tempfile::tempdir().unwrap();
        let am = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));
        let fresh = EchoAgentAuth {
            key: "fresh-from-auth-manager".into(),
            auth_mode: AuthMode::ApiKey,
            create_time: Utc::now(),
            user_id: "user-42".into(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            ..EchoAgentAuth::test_default()
        };
        am.hot_swap(fresh);

        let client = FeedbackClient::new(
            format!("http://{addr}/v1"),
            Some("STALE-build-time-token".into()),
        )
        .with_auth_manager(am.clone());

        let _ = client.get_feedback_config().await;

        let sent = captured.lock().clone().expect("server saw the request");
        assert_eq!(
            sent, "Bearer fresh-from-auth-manager",
            "outgoing bearer must come from AuthManager (not the build-time snapshot)"
        );
    }

    /// Counts refresh() calls -- proves disk-reload short-circuits
    /// before the IdP is hit.
    struct CountingRefresher {
        calls: Arc<AtomicU32>,
    }

    #[async_trait::async_trait]
    impl crate::auth::refresh::TokenRefresher for CountingRefresher {
        async fn refresh(
            &self,
            _reason: crate::auth::refresh::RefreshReason,
        ) -> crate::auth::refresh::RefreshOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            crate::auth::refresh::RefreshOutcome::Success(Box::new(EchoAgentAuth {
                key: "fresh-from-refresher".into(),
                auth_mode: AuthMode::Oidc,
                create_time: Utc::now(),
                user_id: "user-42".into(),
                refresh_token: Some("rt-fresh".into()),
                expires_at: Some(Utc::now() + Duration::hours(1)),
                oidc_issuer: Some("https://issuer.example".into()),
                oidc_client_id: Some("test-client".into()),
                ..EchoAgentAuth::test_default()
            }))
        }
    }

    /// Disk has a fresher RT than memory; recovery must succeed via
    /// reload without calling the refresher.
    #[tokio::test]
    async fn try_refresh_credentials_picks_up_disk_rotation_without_hitting_idp() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = EchoAgentComConfig::default();
        let scope = cfg.auth_scope();
        let am = Arc::new(AuthManager::new(dir.path(), cfg));

        // In-memory: stale token (the one the server rejected).
        am.hot_swap(EchoAgentAuth {
            key: "stale-rejected".into(),
            auth_mode: AuthMode::Oidc,
            create_time: Utc::now() - Duration::hours(2),
            user_id: "user-42".into(),
            refresh_token: Some("rt-stale".into()),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            oidc_issuer: Some("https://issuer.example".into()),
            oidc_client_id: Some("test-client".into()),
            ..EchoAgentAuth::test_default()
        });

        // Disk: a sibling already rotated to a fresh token.
        let disk_auth = EchoAgentAuth {
            key: "fresh-from-sibling-on-disk".into(),
            auth_mode: AuthMode::Oidc,
            create_time: Utc::now(),
            user_id: "user-42".into(),
            refresh_token: Some("rt-fresh".into()),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            oidc_issuer: Some("https://issuer.example".into()),
            oidc_client_id: Some("test-client".into()),
            ..EchoAgentAuth::test_default()
        };
        let mut store = std::collections::BTreeMap::new();
        store.insert(scope, disk_auth);
        let json = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(dir.path().join("auth.json"), json).unwrap();

        let calls = Arc::new(AtomicU32::new(0));
        let refresher = Arc::new(CountingRefresher {
            calls: calls.clone(),
        });
        am.set_refresher(refresher);

        let client = FeedbackClient::new("http://example/v1", Some("stale-rejected".into()))
            .with_auth_manager(am.clone());

        let recovered = client.try_refresh_credentials().await;
        assert!(recovered, "recovery must succeed via disk reload");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "refresher must NOT be called when disk already holds a fresh token \
             (proves we routed through unauthorized_recovery, not direct refresh)"
        );
        assert_eq!(
            am.current().unwrap().key,
            "fresh-from-sibling-on-disk",
            "AuthManager's current token must be the disk-loaded one after recovery"
        );
    }

    /// LegacySession -> `ServerRejectedNoRecovery` -> `false`
    /// (caller stops retrying, doesn't loop on a no-op refresher).
    #[tokio::test]
    async fn try_refresh_credentials_returns_false_on_terminal_failure() {
        let dir = tempfile::tempdir().unwrap();
        let am = Arc::new(AuthManager::new(dir.path(), EchoAgentComConfig::default()));

        // LegacySession: no refresh_token, no recovery possible.
        am.hot_swap(EchoAgentAuth {
            key: "legacy-rejected".into(),
            auth_mode: AuthMode::WebLogin,
            create_time: Utc::now() - Duration::days(60),
            user_id: "user-42".into(),
            ..EchoAgentAuth::test_default()
        });

        let client = FeedbackClient::new("http://example/v1", Some("legacy-rejected".into()))
            .with_auth_manager(am);

        let recovered = client.try_refresh_credentials().await;
        assert!(
            !recovered,
            "LegacySession must surface ServerRejectedNoRecovery as `false`, \
             not loop on a refresher that can't help"
        );
    }
}
