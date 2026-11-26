//! Remote sandbox client for cli-chat-proxy.
//!
//! This module provides an HTTP client to interact with cli-chat-proxy
//! for managing sandbox sessions and environments via REST API.

use std::sync::Arc;

use crate::auth::{AuthManager, EchoAgentComConfig};
use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

// Re-export sandbox API types from echo-agent-chat-proxy-types for convenience.
// Sorted alphabetically; see sandbox_types.rs for logical grouping.
pub use echo_agent_chat_proxy_types::{
    SandboxCreateEnvironmentRequest, SandboxEnvironment, SandboxEnvironmentResponse,
    SandboxEnvironmentVariable, SandboxEnvironmentWithMetadata, SandboxForkRequest,
    SandboxForkResponse, SandboxForkedSession, SandboxHibernateResponse,
    SandboxListEnvironmentsRequest, SandboxListEnvironmentsResponse,
    SandboxListPreinstalledPackagesResponse, SandboxLogsExitCodes, SandboxLogsResponse,
    SandboxMode, SandboxPreinstalledPackage, SandboxRestoreRequest, SandboxRestoreResponse,
    SandboxSecretInput, SandboxStartRequest, SandboxStartResponse, SandboxStatusResponse,
    SandboxTerminateRequest, SandboxUpdateEnvironmentRequest,
};

// ============================================================================
// Sandbox Client
// ============================================================================

/// HTTP client for interacting with the sandbox API via cli-chat-proxy.
///
/// Path parameters (`session_id`, `environment_id`) are interpolated directly
/// into URLs without percent-encoding. This is safe because these IDs are
/// UUIDs in practice. If ID formats ever change to include URL-unsafe
/// characters, the `format!()` calls should be updated to use percent-encoding.
pub struct SandboxClient {
    client: reqwest::Client,
    base_url: String,
    auth_manager: Arc<AuthManager>,
}

impl SandboxClient {
    pub fn new(base_url: impl Into<String>, auth_manager: Arc<AuthManager>) -> Self {
        Self {
            client: crate::http::shared_client(),
            base_url: base_url.into(),
            auth_manager,
        }
    }

    /// Returns the base URL.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    // Do not set Content-Type — callers use .json() and reqwest .header() appends.
    async fn auth_headers(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        let auth = self
            .auth_manager
            .auth()
            .await
            .context("failed to resolve sandbox auth")?;
        let mut builder = builder
            .header("Authorization", format!("Bearer {}", &auth.key))
            .header(
                "X-Echo-Agent-Token-Auth",
                EchoAgentComConfig::default().token_header,
            )
            .header("x-userid", &auth.user_id)
            .header("x-echo-agent-client-version", echo_agent_version::VERSION);

        if let Some(email) = &auth.email {
            builder = builder.header("x-email", email);
        }

        builder = builder
            .header(
                "x-echo-agent-client-identifier",
                crate::http::process_client_identifier(),
            )
            .header(
                crate::http::CLIENT_MODE_HEADER,
                crate::http::process_client_mode(),
            );

        Ok(echo_agent_file_utils::trace_context::inject_trace_context_into_request(builder))
    }

    /// Check an HTTP response for errors, then deserialize the JSON body.
    async fn parse_response<T: DeserializeOwned>(
        response: reqwest::Response,
        operation: &str,
    ) -> Result<T> {
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            bail!("{operation} failed: {status} - {body}");
        }
        response
            .json()
            .await
            .with_context(|| format!("failed to parse {operation} response"))
    }

    /// Check an HTTP response for errors, discarding the body.
    async fn check_response(response: reqwest::Response, operation: &str) -> Result<()> {
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            bail!("{operation} failed: {status} - {body}");
        }
        Ok(())
    }

    /// Fork an existing sandbox session.
    pub async fn fork_session(&self, request: &SandboxForkRequest) -> Result<SandboxForkResponse> {
        let url = format!("{}/sandbox/sessions/fork", self.base_url);
        let response = self
            .auth_headers(self.client.post(&url))
            .await?
            .json(request)
            .send()
            .await
            .context("failed to send fork session request")?;
        Self::parse_response(response, "fork session").await
    }

    /// Terminate a sandbox session.
    pub(crate) async fn terminate_session(
        &self,
        session_id: &str,
        request: &SandboxTerminateRequest,
    ) -> Result<()> {
        let mut url = format!("{}/sandbox/sessions/{}", self.base_url, session_id);
        if let Some(env_id) = &request.environment_id {
            url = format!("{}?environmentId={}", url, env_id);
        }

        let response = self
            .auth_headers(self.client.delete(&url))
            .await?
            .send()
            .await
            .context("failed to send terminate session request")?;

        if response.status().as_u16() == 404 {
            bail!("session not found: {session_id}");
        }
        Self::check_response(response, "terminate session").await
    }

    // ========================================================================
    // Session Lifecycle
    // ========================================================================

    // ========================================================================
    // Environment CRUD
    // ========================================================================

    /// List sandbox environments.
    pub async fn list_environments(
        &self,
        request: &SandboxListEnvironmentsRequest,
    ) -> Result<SandboxListEnvironmentsResponse> {
        let url = format!("{}/sandbox/environments", self.base_url);
        let mut builder = self.auth_headers(self.client.get(&url)).await?;
        if let Some(page) = request.page {
            builder = builder.query(&[("page", page)]);
        }
        if let Some(page_size) = request.page_size {
            builder = builder.query(&[("pageSize", page_size)]);
        }
        let response = builder
            .send()
            .await
            .context("failed to send list environments request")?;
        Self::parse_response(response, "list environments").await
    }

    /// Create a new sandbox environment.
    pub(crate) async fn create_environment(
        &self,
        request: &SandboxCreateEnvironmentRequest,
    ) -> Result<SandboxEnvironmentResponse> {
        let url = format!("{}/sandbox/environments", self.base_url);
        let response = self
            .auth_headers(self.client.post(&url))
            .await?
            .json(request)
            .send()
            .await
            .context("failed to send create environment request")?;
        Self::parse_response(response, "create environment").await
    }

    /// Update a sandbox environment.
    pub(crate) async fn update_environment(
        &self,
        environment_id: &str,
        request: &SandboxUpdateEnvironmentRequest,
    ) -> Result<SandboxEnvironmentResponse> {
        let url = format!("{}/sandbox/environments/{}", self.base_url, environment_id);
        let response = self
            .auth_headers(self.client.put(&url))
            .await?
            .json(request)
            .send()
            .await
            .context("failed to send update environment request")?;
        Self::parse_response(response, "update environment").await
    }

    /// Delete a sandbox environment.
    pub(crate) async fn delete_environment(&self, environment_id: &str) -> Result<()> {
        let url = format!("{}/sandbox/environments/{}", self.base_url, environment_id);
        let response = self
            .auth_headers(self.client.delete(&url))
            .await?
            .send()
            .await
            .context("failed to send delete environment request")?;
        Self::check_response(response, "delete environment").await
    }
}
