//! Managed MCP gateway catalog and tool calls via the EchoAgent API.
//!
//! Catalog: `GET /v1/mcp/tools/list` → `managed_gateway:*` rows.
//! Call: `POST /v1/mcp/tools/call`.
//!
//! Config-file/plugin merge (which reads shell's config system) lives
//! in shell's `session::managed_mcp`, which re-exports everything here.

use std::collections::HashSet;
use std::sync::Arc;

/// Agent-level cache for managed MCP gateway tool catalogs.
pub enum GatewayToolCatalogCache {
    NotFetched,
    /// Fetch in progress for the recorded gateway tool epoch.
    Fetching(u64),
    /// May be empty if the user has no gateway-exposed tools.
    Ready(GatewayToolCatalog),
}

pub struct ManagedMcpState {
    pub gateway_tools_active: bool,
    pub gateway_tool_epoch: u64,
    pub gateway_tool_cache: GatewayToolCatalogCache,
    pub gateway_tool_fetch_notify: Arc<tokio::sync::Notify>,
    /// Retained across gateway disable/cache invalidation so the on-disk
    /// MCP descriptor mirror can remove stale gateway connector directories when
    /// the current catalog is empty or absent.
    pub gateway_tool_connectors_seen: HashSet<String>,
}

impl Default for ManagedMcpState {
    fn default() -> Self {
        Self {
            gateway_tools_active: false,
            gateway_tool_epoch: 0,
            gateway_tool_cache: GatewayToolCatalogCache::NotFetched,
            gateway_tool_fetch_notify: Arc::new(tokio::sync::Notify::new()),
            gateway_tool_connectors_seen: HashSet::new(),
        }
    }
}

impl ManagedMcpState {
    pub fn enable_gateway_tools(&mut self) -> u64 {
        if !self.gateway_tools_active {
            self.gateway_tool_epoch = self.gateway_tool_epoch.wrapping_add(1);
        }
        self.gateway_tools_active = true;
        self.gateway_tool_epoch
    }

    pub fn start_gateway_tool_fetch(&mut self) -> Option<u64> {
        if !self.gateway_tools_active {
            return None;
        }
        self.gateway_tool_cache = GatewayToolCatalogCache::Fetching(self.gateway_tool_epoch);
        Some(self.gateway_tool_epoch)
    }

    pub fn complete_gateway_tool_fetch(&mut self, epoch: u64, catalog: GatewayToolCatalog) -> bool {
        if !self.gateway_tools_active || self.gateway_tool_epoch != epoch {
            self.gateway_tool_fetch_notify.notify_waiters();
            return false;
        }
        self.gateway_tool_connectors_seen
            .extend(catalog.tools.iter().map(|tool| tool.connector_id.clone()));
        self.gateway_tool_cache = GatewayToolCatalogCache::Ready(catalog);
        self.gateway_tool_fetch_notify.notify_waiters();
        true
    }

    pub fn fail_gateway_tool_fetch(&mut self, epoch: u64) {
        if self.gateway_tools_active
            && self.gateway_tool_epoch == epoch
            && matches!(self.gateway_tool_cache, GatewayToolCatalogCache::Fetching(fetch_epoch) if fetch_epoch == epoch)
        {
            self.gateway_tool_cache = GatewayToolCatalogCache::NotFetched;
        }
        self.gateway_tool_fetch_notify.notify_waiters();
    }

    pub fn disable_gateway_tools(&mut self) {
        self.gateway_tools_active = false;
        self.gateway_tool_epoch = self.gateway_tool_epoch.wrapping_add(1);
        self.gateway_tool_cache = GatewayToolCatalogCache::NotFetched;
        self.gateway_tool_fetch_notify.notify_waiters();
    }
}

pub type ManagedMcpStateHandle = Arc<tokio::sync::Mutex<ManagedMcpState>>;

#[derive(Debug, Clone, serde::Serialize)]
pub struct GatewayToolCallRequest {
    pub call_id: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GatewayToolCallResponse {
    pub result: serde_json::Value,
    #[serde(default)]
    pub connectors_needing_reauth: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GatewayToolCatalog {
    #[serde(default)]
    pub tools: Vec<GatewayTool>,
    #[serde(default)]
    pub total_tools: u32,
    #[serde(default)]
    pub connectors_needing_reauth: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct GatewayTool {
    pub connector_id: String,
    pub connector_name: String,
    pub tool_id: String,
    pub tool_name: String,
    pub call_id: String,
    pub description: String,
    pub json_schema: serde_json::Value,
}

impl GatewayTool {
    pub fn qualified_name(&self) -> String {
        format!("{}__{}", self.connector_id, self.tool_id)
    }
}

/// Why a managed-MCP gateway fetch failed. Distinguishes "fetch failed" from
/// the legitimate "fetched, zero connectors" (`Ok` with an empty catalog) so
/// the agent cache never commits a transient failure as a permanent empty
/// catalog.
#[derive(Debug, thiserror::Error)]
pub enum ManagedMcpFetchError {
    #[error("HTTP {status}: {message}")]
    Status {
        status: reqwest::StatusCode,
        message: String,
    },
    #[error("transport: {0}")]
    Transport(#[from] reqwest::Error),
    /// No usable auth token at fetch time.
    #[error("no auth token available")]
    NoAuth,
}

async fn get_authenticated_json<T: serde::de::DeserializeOwned>(
    url: &str,
    auth_key: &str,
    unavailable_message: &'static str,
    fetch_failed_message: &'static str,
    parse_error_message: &'static str,
) -> Result<T, ManagedMcpFetchError> {
    let resp = match echo_agent_http::shared_client()
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .header("Authorization", format!("Bearer {auth_key}"))
        .header("X-Echo-Agent-Token-Auth", "echo-agent-cli")
        .header("x-echo-agent-client-version", echo_agent_version::VERSION)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status();
            tracing::warn!(status = %status, "{}", unavailable_message);
            return Err(ManagedMcpFetchError::Status {
                status,
                message: format!("HTTP {status}"),
            });
        }
        Err(e) => {
            tracing::warn!(error = %e, "{}", fetch_failed_message);
            return Err(e.into());
        }
    };

    match resp.json::<T>().await {
        Ok(value) => Ok(value),
        Err(e) => {
            tracing::debug!(error = %e, "{}", parse_error_message);
            Err(e.into())
        }
    }
}

// Above the server-side tool-call budget so the client is not the first
// hop to abort a slow tool call.
const GATEWAY_TOOL_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(75);

pub async fn call_gateway_tool(
    proxy_base_url: &str,
    auth_key: &str,
    call_id: &str,
    arguments: serde_json::Value,
) -> Result<GatewayToolCallResponse, ManagedMcpFetchError> {
    let url = format!("{proxy_base_url}/mcp/tools/call");
    let arguments = if arguments.is_null() {
        serde_json::json!({})
    } else {
        arguments
    };
    let request = GatewayToolCallRequest {
        call_id: call_id.to_owned(),
        arguments,
    };

    let resp = match echo_agent_http::shared_client()
        .post(&url)
        .timeout(GATEWAY_TOOL_CALL_TIMEOUT)
        .header("Authorization", format!("Bearer {auth_key}"))
        .header("X-Echo-Agent-Token-Auth", "echo-agent-cli")
        .header("x-echo-agent-client-version", echo_agent_version::VERSION)
        .json(&request)
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            let status = r.status();
            let message = gateway_error_message(status, r).await;
            tracing::warn!(
                call_id = %call_id,
                "Managed MCP gateway tool call unavailable: HTTP {status}"
            );
            return Err(ManagedMcpFetchError::Status { status, message });
        }
        Err(e) => {
            tracing::warn!(
                call_id = %call_id,
                "Managed MCP gateway tool call failed: {}",
                e
            );
            return Err(e.into());
        }
    };

    match resp.json::<GatewayToolCallResponse>().await {
        Ok(response) => Ok(response),
        Err(e) => {
            tracing::debug!(
                call_id = %call_id,
                "Managed MCP gateway tool call parse error: {}",
                e
            );
            Err(e.into())
        }
    }
}

async fn gateway_error_message(status: reqwest::StatusCode, response: reqwest::Response) -> String {
    let fallback = format!("HTTP {status}");
    let Ok(body) = response.text().await else {
        return fallback;
    };
    if body.trim().is_empty() {
        return fallback;
    }
    match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(value) => value
            .get("error")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .unwrap_or(fallback),
        Err(_) => fallback,
    }
}

/// Fetch the managed MCP gateway tool catalog from the EchoAgent API
/// (`GET /v1/mcp/tools/list`).
///
/// `Ok(catalog)` means the server answered and the catalog contents are
/// authoritative for this fetch, even when empty. `Err(_)` means freshness is
/// unknown and callers must leave any cache retryable rather than committing an
/// empty catalog.
pub async fn fetch_gateway_tool_catalog(
    proxy_base_url: &str,
    auth_key: &str,
) -> Result<GatewayToolCatalog, ManagedMcpFetchError> {
    let url = format!("{proxy_base_url}/mcp/tools/list");

    let catalog: GatewayToolCatalog = get_authenticated_json(
        &url,
        auth_key,
        "Managed MCP gateway tools unavailable",
        "Managed MCP gateway tools fetch failed",
        "Managed MCP gateway tools parse error",
    )
    .await?;
    tracing::info!(
        count = catalog.tools.len(),
        total_tools = catalog.total_tools,
        reauth = catalog.connectors_needing_reauth.len(),
        "Fetched managed MCP gateway tool catalog"
    );
    Ok(catalog)
}

/// Invalidate only the gateway tool catalog so the next gateway-aware caller
/// refetches `/v1/mcp/tools/list`.
pub async fn invalidate_gateway_tool_cache(handle: &ManagedMcpStateHandle) {
    let mut state = handle.lock().await;
    state.gateway_tool_cache = GatewayToolCatalogCache::NotFetched;
}

/// Fetch-or-wait for the managed MCP gateway tool catalog.
///
/// Returns `Some(catalog)` for either a cached catalog or a successful fresh
/// fetch, including a genuine empty catalog. Returns `None` when gateway tools
/// are disabled by the caller, auth is unavailable, or the fetch failed. Failed
/// fetches roll back to `NotFetched`, so a later caller can retry.
pub async fn get_or_fetch_gateway_tool_catalog(
    handle: &ManagedMcpStateHandle,
    proxy_url: &str,
    auth_key: Option<&str>,
) -> Option<GatewayToolCatalog> {
    let fetch_epoch = loop {
        let maybe_notify = {
            let mut state = handle.lock().await;
            if !state.gateway_tools_active {
                return None;
            }
            match &state.gateway_tool_cache {
                GatewayToolCatalogCache::Ready(catalog) => return Some(catalog.clone()),
                GatewayToolCatalogCache::Fetching(_) => {
                    Some(state.gateway_tool_fetch_notify.clone().notified_owned())
                }
                GatewayToolCatalogCache::NotFetched => {
                    let epoch = state.start_gateway_tool_fetch()?;
                    break epoch;
                }
            }
        };

        if let Some(notified) = maybe_notify {
            notified.await;
            continue;
        }
    };

    let result = match auth_key {
        Some(key) => fetch_gateway_tool_catalog(proxy_url, key).await,
        None => Err(ManagedMcpFetchError::NoAuth),
    };

    match result {
        Ok(catalog) => {
            let committed = handle
                .lock()
                .await
                .complete_gateway_tool_fetch(fetch_epoch, catalog.clone());
            committed.then_some(catalog)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "Managed MCP gateway tool fetch failed; leaving cache unpopulated for retry"
            );
            handle.lock().await.fail_gateway_tool_fetch(fetch_epoch);
            None
        }
    }
}

pub fn normalize_url(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_url_strips_trailing_slash() {
        assert_eq!(
            normalize_url("https://mcp.example.com/sse/"),
            "https://mcp.example.com/sse"
        );
        assert_eq!(
            normalize_url("https://mcp.example.com/sse"),
            "https://mcp.example.com/sse"
        );
    }

    #[test]
    fn gateway_tool_catalog_deserializes() {
        let catalog: GatewayToolCatalog = serde_json::from_str(
            r#"{
            "tools": [
                {
                    "connector_id": "gmail",
                    "connector_name": "Gmail",
                    "tool_id": "search",
                    "tool_name": "Search Gmail",
                    "call_id": "gmail_search",
                    "description": "Search email by query",
                    "json_schema": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string" }
                        },
                        "required": ["query"]
                    }
                }
            ],
            "total_tools": 1,
            "connectors_needing_reauth": ["Slack"]
        }"#,
        )
        .unwrap();

        assert_eq!(1, catalog.total_tools);
        let without_total_tools: GatewayToolCatalog = serde_json::from_str(
            r#"{
            "tools": [],
            "connectors_needing_reauth": []
        }"#,
        )
        .unwrap();
        assert_eq!(0, without_total_tools.total_tools);
        assert_eq!(vec!["Slack"], catalog.connectors_needing_reauth);
        assert_eq!("gmail_search", catalog.tools[0].call_id);
        assert_eq!("gmail__search", catalog.tools[0].qualified_name());
        assert_eq!("gmail", catalog.tools[0].connector_id);
        assert_eq!("Gmail", catalog.tools[0].connector_name);
        assert_eq!("search", catalog.tools[0].tool_id);
        assert_eq!("Search Gmail", catalog.tools[0].tool_name);
        assert_eq!(
            Some("string"),
            catalog.tools[0]
                .json_schema
                .pointer("/properties/query/type")
                .and_then(|v| v.as_str())
        );
    }

    #[tokio::test]
    async fn gateway_tool_call_error_preserves_proxy_message() {
        use axum::Router;
        use axum::routing::post;
        use tokio::net::TcpListener;

        let app = Router::new().route(
            "/mcp/tools/call",
            post(|| async {
                (
                    axum::http::StatusCode::BAD_REQUEST,
                    axum::Json(serde_json::json!({
                        "code": "Client specified an invalid argument",
                        "error": "Invalid arguments for google_calendar_availability: missing field `calendars`"
                    })),
                )
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let err = call_gateway_tool(
            &format!("http://{addr}"),
            "token",
            "google_calendar_availability",
            serde_json::json!({}),
        )
        .await
        .unwrap_err();

        match err {
            ManagedMcpFetchError::Status { status, message } => {
                assert_eq!(reqwest::StatusCode::BAD_REQUEST, status);
                assert_eq!(
                    "Invalid arguments for google_calendar_availability: missing field `calendars`",
                    message
                );
            }
            other => panic!("expected status error, got {other:?}"),
        }
    }

    #[test]
    fn disable_gateway_tools_clears_cached_catalog() {
        let mut state = ManagedMcpState::default();
        state.enable_gateway_tools();
        let epoch = state.start_gateway_tool_fetch().unwrap();
        assert!(state.complete_gateway_tool_fetch(
            epoch,
            GatewayToolCatalog {
                tools: vec![],
                total_tools: 0,
                connectors_needing_reauth: vec![],
            }
        ));
        assert!(state.gateway_tools_active);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::Ready(_)
        ));

        state.disable_gateway_tools();
        assert!(!state.gateway_tools_active);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::NotFetched
        ));
    }

    #[test]
    fn stale_gateway_tool_fetch_success_does_not_commit_after_disable() {
        let mut state = ManagedMcpState::default();
        state.enable_gateway_tools();
        let epoch = state.start_gateway_tool_fetch().unwrap();
        state.disable_gateway_tools();

        let committed = state.complete_gateway_tool_fetch(
            epoch,
            GatewayToolCatalog {
                tools: vec![],
                total_tools: 0,
                connectors_needing_reauth: vec![],
            },
        );

        assert!(!committed);
        assert!(!state.gateway_tools_active);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::NotFetched
        ));
    }

    #[tokio::test]
    async fn gateway_tool_waiter_woken_by_disable_does_not_reenable() {
        let handle = ManagedMcpStateHandle::default();
        {
            let mut state = handle.lock().await;
            state.enable_gateway_tools();
            state.start_gateway_tool_fetch().unwrap();
        }
        let waiter_handle = handle.clone();
        let waiter = tokio::spawn(async move {
            get_or_fetch_gateway_tool_catalog(&waiter_handle, "http://127.0.0.1:0", Some("token"))
                .await
        });

        tokio::task::yield_now().await;
        handle.lock().await.disable_gateway_tools();
        let catalog = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("waiter must wake after disable")
            .expect("waiter task should not panic");
        assert!(catalog.is_none());
        let state = handle.lock().await;
        assert!(!state.gateway_tools_active);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::NotFetched
        ));
    }

    #[tokio::test]
    async fn failed_gateway_tool_fetch_is_not_cached_as_ready_empty() {
        let handle = ManagedMcpStateHandle::default();
        let catalog = get_or_fetch_gateway_tool_catalog(&handle, "http://127.0.0.1:0", None).await;
        assert!(catalog.is_none());
        assert!(
            matches!(
                handle.lock().await.gateway_tool_cache,
                GatewayToolCatalogCache::NotFetched
            ),
            "failed gateway tool fetch must roll back to NotFetched, not poison the cache as Ready(empty)"
        );
    }

    #[test]
    fn failed_gateway_tool_fetch_does_not_clear_ready_catalog_from_same_epoch() {
        let mut state = ManagedMcpState::default();
        state.enable_gateway_tools();
        let epoch = state.start_gateway_tool_fetch().unwrap();
        assert!(state.complete_gateway_tool_fetch(
            epoch,
            GatewayToolCatalog {
                tools: vec![],
                total_tools: 0,
                connectors_needing_reauth: vec![],
            },
        ));

        state.fail_gateway_tool_fetch(epoch);
        assert!(matches!(
            state.gateway_tool_cache,
            GatewayToolCatalogCache::Ready(_)
        ));
    }

    #[tokio::test]
    async fn successful_gateway_tool_fetch_is_cached_ready() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app_calls = calls.clone();
        let app = axum::Router::new().route(
            "/mcp/tools/list",
            axum::routing::get(move || {
                app_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    axum::Json(serde_json::json!({
                        "tools": [
                            {
                                "connector_id": "gmail",
                                "connector_name": "Gmail",
                                "tool_id": "search",
                                "tool_name": "Search Gmail",
                                "call_id": "gmail_search",
                                "description": "Search email by query",
                                "json_schema": { "type": "object" }
                            }
                        ],
                        "total_tools": 1,
                        "connectors_needing_reauth": []
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let handle = ManagedMcpStateHandle::default();
        handle.lock().await.enable_gateway_tools();
        let catalog = get_or_fetch_gateway_tool_catalog(&handle, &base_url, Some("token"))
            .await
            .expect("gateway catalog fetch should succeed");
        assert_eq!("gmail__search", catalog.tools[0].qualified_name());
        assert!(matches!(
            handle.lock().await.gateway_tool_cache,
            GatewayToolCatalogCache::Ready(_)
        ));

        let cached =
            get_or_fetch_gateway_tool_catalog(&handle, "http://127.0.0.1:0", Some("token"))
                .await
                .expect("second call should use cached catalog");
        assert_eq!("gmail_search", cached.tools[0].call_id);
        assert_eq!(1, calls.load(std::sync::atomic::Ordering::SeqCst));
        server.abort();
    }

    #[tokio::test]
    async fn gateway_tool_fetch_waiter_survives_notify_before_await() {
        let handle = ManagedMcpStateHandle::default();
        let (epoch, registered) = {
            let mut state = handle.lock().await;
            state.enable_gateway_tools();
            let epoch = state.start_gateway_tool_fetch().unwrap();
            (
                epoch,
                state.gateway_tool_fetch_notify.clone().notified_owned(),
            )
        };
        handle.lock().await.fail_gateway_tool_fetch(epoch);
        tokio::time::timeout(std::time::Duration::from_secs(1), registered)
            .await
            .expect("registered gateway catalog waiter must observe notify_waiters");
    }

    #[tokio::test]
    async fn invalidate_gateway_tool_cache_forces_refetch() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app_calls = calls.clone();
        let app = axum::Router::new().route(
            "/mcp/tools/list",
            axum::routing::get(move || {
                app_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    axum::Json(serde_json::json!({
                        "tools": [],
                        "total_tools": 0,
                        "connectors_needing_reauth": []
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let handle = ManagedMcpStateHandle::default();
        handle.lock().await.enable_gateway_tools();
        assert!(
            get_or_fetch_gateway_tool_catalog(&handle, &base_url, Some("token"))
                .await
                .is_some()
        );
        invalidate_gateway_tool_cache(&handle).await;
        assert!(
            get_or_fetch_gateway_tool_catalog(&handle, &base_url, Some("token"))
                .await
                .is_some()
        );
        assert_eq!(2, calls.load(std::sync::atomic::Ordering::SeqCst));
        server.abort();
    }
}
