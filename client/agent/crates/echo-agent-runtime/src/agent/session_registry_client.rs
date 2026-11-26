//! REST client for the session replicas registry (cli-chat-proxy).
//!
//! Handles registering, updating, finalizing, searching, and downloading
//! session replicas for cross-host session replication. Write methods
//! (register/update/finalize) are fire-and-forget safe. Read methods
//! (search/get/download_file) return typed results.

use anyhow::{Context, Result};
use reqwest::RequestBuilder;
use serde::{Deserialize, Serialize};

// ============================================================================
// Request / response types (local — not in cli-chat-proxy since these
// are only used by the agent, not consumed by other crates)
// ============================================================================

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub session_id: String,
    pub cwd: String,
    pub gcs_trace_prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_remote_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_head_at_start: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Opaque per-machine device id (telemetry `agent_id()`) for machine disambiguation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    // --- Subagent-specific fields (optional, backward-compatible) ---
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_persona: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_context_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_depth: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_turn_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_head_at_end: Option<String>,
    /// Latest turn whose restore artifacts are confirmed durable.
    /// Omitted from the wire when `None` — old servers ignore unknown fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restorable_turn_number: Option<i32>,
}

// ============================================================================
// Response types
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub session_id: String,
    pub summary: String,
    pub first_prompt: Option<String>,
    pub model_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_turn_number: i32,
    /// Present on servers that have applied the restorable-turn migration.
    /// `None` when talking to an older server — callers should fall back to
    /// `last_turn_number` in that case.
    #[serde(default)]
    pub restorable_turn_number: Option<i32>,
    pub cwd: String,
    pub repo_remote_url: Option<String>,
    pub hostname: Option<String>,
    pub status: String,
    pub gcs_trace_prefix: String,
    pub gcs_bucket: String,
    #[serde(default)]
    pub last_active_at: Option<String>,
}

impl From<crate::session::persistence::Summary> for SessionRecord {
    fn from(s: crate::session::persistence::Summary) -> Self {
        Self {
            session_id: s.info.id.to_string(),
            summary: s.session_summary,
            first_prompt: None,
            model_id: Some(s.current_model_id.to_string()),
            created_at: s.created_at.to_rfc3339(),
            updated_at: s.updated_at.to_rfc3339(),
            last_turn_number: s.num_messages as i32,
            restorable_turn_number: None,
            cwd: s.info.cwd,
            repo_remote_url: None,
            hostname: None,
            status: "local".to_string(),
            gcs_trace_prefix: String::new(),
            gcs_bucket: String::new(),
            last_active_at: s.last_active_at.map(|t| t.to_rfc3339()),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResponse {
    pub sessions: Vec<SessionRecord>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadResponse {
    pub download_url: String,
    pub file: String,
    pub turn: i32,
}

// ============================================================================
// Client
// ============================================================================

#[derive(Clone)]
pub struct SessionRegistryClient {
    raw_client: reqwest::Client,
    client: reqwest_middleware::ClientWithMiddleware,
    base_url: String,
    credentials: crate::util::echo_agent_auth_credentials::EchoAgentAuthCredentials,
    session_id: Option<String>,
}

impl SessionRegistryClient {
    pub fn new(base_url: impl Into<String>, user_token: impl Into<String>) -> Self {
        let http_client = crate::http::shared_client();
        Self {
            raw_client: http_client.clone(),
            client: reqwest_middleware::ClientBuilder::new(http_client).build(),
            base_url: base_url.into(),
            credentials: crate::util::echo_agent_auth_credentials::EchoAgentAuthCredentials::new(
                Some(user_token.into()),
            ),
            session_id: None,
        }
    }

    pub fn with_deployment_key(mut self, key: Option<String>) -> Self {
        self.credentials.deployment_key = key;
        self
    }

    pub fn with_alpha_test_key(mut self, key: Option<String>) -> Self {
        self.credentials.alpha_test_key = key;
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    /// Attach an `AuthManager` so the request signing and 401
    /// recovery go through the consolidated auth path.
    pub fn with_auth(mut self, auth_manager: std::sync::Arc<crate::auth::AuthManager>) -> Self {
        let provider: std::sync::Arc<dyn echo_agent_auth::AuthCredentialProvider> =
            std::sync::Arc::new(
                crate::auth::credential_provider::ShellAuthCredentialProvider::new(
                    auth_manager.clone(),
                    self.credentials.deployment_key.clone(),
                    self.credentials.alpha_test_key.clone(),
                ),
            );
        self.credentials = self.credentials.with_auth_manager(auth_manager);
        self.client = crate::http::with_auth_retry(self.raw_client.clone(), provider);
        self
    }

    /// Execute with auth middleware, returning the response plus the
    /// bearer suffix the middleware stamped (for truthful 401
    /// attribution in [`Self::check_response`]).
    async fn send_authed(
        &self,
        builder: RequestBuilder,
        op: &'static str,
    ) -> Result<(
        reqwest::Response,
        Option<echo_agent_auth::StampedBearerSuffix>,
    )> {
        let builder =
            echo_agent_file_utils::trace_context::inject_trace_context_into_request(builder);
        let request = builder.build().context(op)?;
        echo_agent_auth::execute_with_stamp(&self.client, request)
            .await
            .map_err(|e| match e {
                reqwest_middleware::Error::Middleware(e) => e.context(op),
                reqwest_middleware::Error::Reqwest(e) => anyhow::Error::from(e).context(op),
            })
    }

    /// Non-auth headers only -- the `Authorization` header lives in
    /// `send_authed` so it picks up freshly-refreshed tokens.
    fn add_common_headers(&self, builder: RequestBuilder) -> RequestBuilder {
        builder
    }

    fn check_response(
        &self,
        response: reqwest::Response,
        stamp: Option<&echo_agent_auth::StampedBearerSuffix>,
        op: &str,
    ) -> anyhow::Error {
        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.record_401_attribution(op, stamp);
            anyhow::anyhow!("{op}: {}", self.credentials.auth_error_hint())
        } else {
            anyhow::anyhow!("{op} failed: {}", response.status())
        }
    }

    /// Emit a single `auth 401 attribution` log entry tagged with
    /// `consumer = "SessionRegistryClient.<op>"`. The op string is the
    /// operation name passed to `check_response` (e.g.,
    /// `"session register"`).
    ///
    /// `stamp` is what the middleware put on the wire (see
    /// [`echo_agent_auth::StampedBearerSuffix`] for why never a re-resolution).
    fn record_401_attribution(
        &self,
        op: &str,
        stamp: Option<&echo_agent_auth::StampedBearerSuffix>,
    ) {
        if let Some(manager) = self.credentials.auth_manager() {
            crate::auth::attribution::record_consumer_401(
                manager.as_ref(),
                self.session_id.as_deref(),
                crate::auth::attribution::ConsumerKind::SessionRegistryClient,
                op,
                stamp.map(|s| s.0.as_str()),
            );
        }
    }

    fn post(&self, url: &str) -> RequestBuilder {
        self.add_common_headers(self.raw_client.post(url))
    }

    fn get(&self, url: &str) -> RequestBuilder {
        self.add_common_headers(self.raw_client.get(url))
    }

    /// POST /v1/sessions/register (idempotent via ON CONFLICT)
    pub async fn register(&self, req: &RegisterRequest) -> Result<()> {
        let url = format!("{}/sessions/register", self.base_url);
        let (response, stamp) = self
            .send_authed(self.post(&url).json(req), "session register")
            .await?;
        if !response.status().is_success() {
            return Err(self.check_response(response, stamp.as_ref(), "session register"));
        }
        Ok(())
    }

    /// POST /v1/sessions/{id}/replicas/update
    pub async fn update(&self, session_id: &str, req: &UpdateRequest) -> Result<()> {
        let url = format!("{}/sessions/{}/replicas/update", self.base_url, session_id);
        let (response, stamp) = self
            .send_authed(self.post(&url).json(req), "session update")
            .await?;
        if !response.status().is_success() {
            return Err(self.check_response(response, stamp.as_ref(), "session update"));
        }
        Ok(())
    }

    /// POST /v1/sessions/{id}/replicas/finalize
    pub async fn finalize(&self, session_id: &str) -> Result<()> {
        let url = format!(
            "{}/sessions/{}/replicas/finalize",
            self.base_url, session_id
        );
        let (response, stamp) = self
            .send_authed(self.post(&url), "session finalize")
            .await?;
        if !response.status().is_success() {
            return Err(self.check_response(response, stamp.as_ref(), "session finalize"));
        }
        Ok(())
    }

    /// GET /v1/sessions/search
    pub async fn search(&self, query: Option<&str>, limit: i64) -> Result<Vec<SessionRecord>> {
        let url = format!("{}/sessions/search", self.base_url);
        let mut builder = self.get(&url).query(&[("limit", limit.to_string())]);
        if let Some(q) = query {
            builder = builder.query(&[("query", q)]);
        }
        let (response, stamp) = self.send_authed(builder, "session search").await?;
        if !response.status().is_success() {
            return Err(self.check_response(response, stamp.as_ref(), "session search"));
        }
        let resp: SearchResponse = response.json().await.context("parse search response")?;
        Ok(resp.sessions)
    }

    /// GET /v1/sessions/{id}/replicas
    pub async fn get_session(&self, session_id: &str) -> Result<SessionRecord> {
        let url = format!("{}/sessions/{}/replicas", self.base_url, session_id);
        let (response, stamp) = self.send_authed(self.get(&url), "session get").await?;
        if !response.status().is_success() {
            return Err(self.check_response(response, stamp.as_ref(), "session get"));
        }
        response.json().await.context("parse session response")
    }

    /// GET /v1/sessions/{id}/download — returns a signed GCS URL without downloading.
    pub(crate) async fn get_download_url(
        &self,
        session_id: &str,
        file: &str,
        turn: i32,
    ) -> Result<String> {
        let url = format!("{}/sessions/{}/download", self.base_url, session_id);
        let builder = self
            .get(&url)
            .query(&[("file", file), ("turn", &turn.to_string())]);
        let (response, stamp) = self.send_authed(builder, "session download url").await?;
        if !response.status().is_success() {
            return Err(self.check_response(response, stamp.as_ref(), "session download url"));
        }
        let resp: DownloadResponse = response.json().await.context("parse download response")?;
        Ok(resp.download_url)
    }

    /// GET /v1/sessions/{id}/download — returns a signed URL, then streams to dest file.
    pub async fn download_file(
        &self,
        session_id: &str,
        file: &str,
        turn: i32,
        dest: &std::path::Path,
    ) -> Result<()> {
        let url = format!("{}/sessions/{}/download", self.base_url, session_id);
        let builder = self
            .get(&url)
            .query(&[("file", file), ("turn", &turn.to_string())]);
        let (response, stamp) = self.send_authed(builder, "session download").await?;
        if !response.status().is_success() {
            return Err(self.check_response(response, stamp.as_ref(), "session download"));
        }
        let resp: DownloadResponse = response.json().await.context("parse download response")?;

        // Stream from the signed GCS URL directly to disk (archives can be hundreds of MB)
        let mut gcs_response = self
            .raw_client
            .get(&resp.download_url)
            .send()
            .await
            .context("download from GCS")?;
        if !gcs_response.status().is_success() {
            anyhow::bail!("GCS download failed: {}", gcs_response.status());
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut out = tokio::fs::File::create(dest)
            .await
            .context("create dest file")?;
        let chunk_timeout = std::time::Duration::from_secs(60);
        loop {
            match tokio::time::timeout(chunk_timeout, gcs_response.chunk()).await {
                Ok(Ok(Some(chunk))) => {
                    tokio::io::AsyncWriteExt::write_all(&mut out, &chunk)
                        .await
                        .context("write chunk to disk")?;
                }
                Ok(Ok(None)) => break,
                Ok(Err(e)) => return Err(e).context("read GCS chunk"),
                Err(_) => anyhow::bail!(
                    "GCS download stalled: no data received for {chunk_timeout:?} \
                     while downloading {file}"
                ),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── UpdateRequest wire shapes ────────────────────────────────────────────
    //
    // The writer split relies on two distinct update payloads being sent at
    // different times:
    //
    //   1. Immediate post-turn: `last_turn_number` + `repo_head_at_end`
    //   2. Artifact-ready:      `restorable_turn_number` only
    //
    // These tests verify that `skip_serializing_if = "Option::is_none"` does the
    // right thing for each shape, so old servers silently ignore the new field and
    // clients don't accidentally overwrite unrelated fields with nulls.

    #[test]
    fn immediate_turn_update_omits_restorable_field() {
        let req = UpdateRequest {
            summary: None,
            first_prompt: None,
            last_turn_number: Some(5),
            repo_head_at_end: Some("abc123".into()),
            restorable_turn_number: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["lastTurnNumber"], 5);
        assert_eq!(json["repoHeadAtEnd"], "abc123");
        assert!(json.get("restorableTurnNumber").is_none());
        assert!(json.get("summary").is_none());
        assert!(json.get("firstPrompt").is_none());
    }

    #[test]
    fn restorable_turn_update_omits_last_turn_and_head_fields() {
        let req = UpdateRequest {
            summary: None,
            first_prompt: None,
            last_turn_number: None,
            repo_head_at_end: None,
            restorable_turn_number: Some(5),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["restorableTurnNumber"], 5);
        assert!(json.get("lastTurnNumber").is_none());
        assert!(json.get("repoHeadAtEnd").is_none());
        assert!(json.get("summary").is_none());
        assert!(json.get("firstPrompt").is_none());
    }

    #[test]
    fn summary_update_omits_all_turn_fields() {
        let req = UpdateRequest {
            summary: Some("My session summary".into()),
            first_prompt: None,
            last_turn_number: None,
            repo_head_at_end: None,
            restorable_turn_number: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["summary"], "My session summary");
        assert!(json.get("lastTurnNumber").is_none());
        assert!(json.get("restorableTurnNumber").is_none());
        assert!(json.get("repoHeadAtEnd").is_none());
    }

    #[test]
    fn empty_summary_is_sent_not_omitted() {
        let req = UpdateRequest {
            summary: Some(String::new()),
            first_prompt: None,
            last_turn_number: None,
            repo_head_at_end: None,
            restorable_turn_number: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(
            json.get("summary"),
            Some(&serde_json::json!("")),
            "unpin must POST summary:\"\" so a merge replica drops the prior title"
        );
    }

    // Wire-contract tests: server reads the camelCase `deviceId` key.

    fn minimal_register_request(device_id: Option<String>) -> RegisterRequest {
        RegisterRequest {
            session_id: "s1".into(),
            cwd: "/x".into(),
            gcs_trace_prefix: "t".into(),
            model_id: None,
            repo_remote_url: None,
            repo_branch: None,
            repo_head_at_start: None,
            hostname: None,
            device_id,
            parent_session_id: None,
            session_kind: None,
            subagent_type: None,
            subagent_persona: None,
            subagent_role: None,
            fork_context_source: None,
            subagent_depth: None,
        }
    }

    #[test]
    fn register_request_serializes_device_id_as_camel_case() {
        let req = minimal_register_request(Some("machine-uuid-123".into()));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["deviceId"], "machine-uuid-123");
        assert!(json.get("device_id").is_none());
    }

    #[test]
    fn register_request_serializes_empty_device_id_as_present() {
        let req = minimal_register_request(Some(String::new()));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["deviceId"], "");
    }

    #[test]
    fn register_request_omits_device_id_when_none() {
        let req = minimal_register_request(None);
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("deviceId").is_none());
        assert!(json.get("device_id").is_none());
    }

    // ── SessionRecord backward compatibility ─────────────────────────────────
    //
    // Older servers do not include `restorable_turn_number` in their response.
    // The field is `#[serde(default)]` so it must deserialize as `None` when
    // absent, keeping new clients compatible with old servers.

    #[test]
    fn session_record_without_restorable_turn_deserializes_as_none() {
        let json = serde_json::json!({
            "sessionId": "sess-abc",
            "summary": "hello",
            "firstPrompt": null,
            "modelId": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "lastTurnNumber": 3,
            "cwd": "/home/user/repo",
            "repoRemoteUrl": null,
            "hostname": null,
            "status": "active",
            "gcsTracePrefix": "sessions/sess-abc",
            "gcsBucket": "my-bucket"
        });
        let record: SessionRecord = serde_json::from_value(json).unwrap();
        assert_eq!(record.last_turn_number, 3);
        assert_eq!(record.restorable_turn_number, None);
    }

    #[test]
    fn session_record_with_restorable_turn_deserializes_correctly() {
        let json = serde_json::json!({
            "sessionId": "sess-xyz",
            "summary": "hello",
            "firstPrompt": null,
            "modelId": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "lastTurnNumber": 7,
            "restorableTurnNumber": 6,
            "cwd": "/home/user/repo",
            "repoRemoteUrl": null,
            "hostname": null,
            "status": "active",
            "gcsTracePrefix": "sessions/sess-xyz",
            "gcsBucket": "my-bucket"
        });
        let record: SessionRecord = serde_json::from_value(json).unwrap();
        assert_eq!(record.last_turn_number, 7);
        assert_eq!(record.restorable_turn_number, Some(6));
    }

    /// Verify per-request auth resolve picks up rotated tokens.
    #[tokio::test]
    async fn session_registry_client_uses_active_auth_for_each_request() {
        use crate::auth::{AuthManager, AuthMode, EchoAgentAuth, EchoAgentComConfig};
        use axum::{Router, response::IntoResponse, routing::post};
        use chrono::{Duration, Utc};
        use std::net::SocketAddr;
        use std::sync::Arc;
        use tokio::net::TcpListener;

        let captured = Arc::new(parking_lot::Mutex::new(None::<String>));
        let captured_for_handler = captured.clone();
        let router = Router::new().route(
            "/sessions/register",
            post(move |headers: axum::http::HeaderMap, _body: String| {
                let captured = captured_for_handler.clone();
                async move {
                    if let Some(auth) = headers.get(axum::http::header::AUTHORIZATION) {
                        *captured.lock() = Some(auth.to_str().unwrap_or("").to_owned());
                    }
                    (axum::http::StatusCode::OK, "").into_response()
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

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

        let client = SessionRegistryClient::new(format!("http://{addr}"), "STALE-build-time-token")
            .with_auth(am);
        let req = RegisterRequest {
            session_id: "s1".into(),
            cwd: "/x".into(),
            gcs_trace_prefix: "t".into(),
            model_id: None,
            repo_remote_url: None,
            repo_branch: None,
            repo_head_at_start: None,
            hostname: None,
            device_id: None,
            parent_session_id: None,
            session_kind: None,
            subagent_type: None,
            subagent_persona: None,
            subagent_role: None,
            fork_context_source: None,
            subagent_depth: None,
        };
        client.register(&req).await.unwrap();

        let sent = captured.lock().clone().expect("server saw the request");
        assert_eq!(
            sent, "Bearer fresh-from-auth-manager",
            "outgoing bearer must come from AuthManager (not the build-time token)"
        );
    }

    // Verify the split-pointer invariant: last_turn_number can be ahead of
    // restorable_turn_number (codebase best-effort means a turn may be "done"
    // but not yet restorable if session-state upload is still in flight).
    #[test]
    fn session_record_allows_last_turn_ahead_of_restorable() {
        let json = serde_json::json!({
            "sessionId": "sess-lag",
            "summary": "",
            "firstPrompt": null,
            "modelId": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "lastTurnNumber": 10,
            "restorableTurnNumber": 8,
            "cwd": "/repo",
            "repoRemoteUrl": null,
            "hostname": null,
            "status": "active",
            "gcsTracePrefix": "sessions/sess-lag",
            "gcsBucket": "bucket"
        });
        let record: SessionRecord = serde_json::from_value(json).unwrap();
        assert!(record.last_turn_number > record.restorable_turn_number.unwrap_or(0));
    }
}
