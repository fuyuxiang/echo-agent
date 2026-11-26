//! Cached HTTP client with atomic invalidation for `web_fetch`.
//!
//! The `reqwest::Client` is held behind an `ArcSwapOption` so it can be
//! atomically invalidated on transport errors, forcing the next call to rebuild
//! with a fresh connection pool. This prevents connection pool poisoning
//! (half-read connections being returned to the pool and corrupting subsequent
//! requests).

use std::sync::Arc;

use arc_swap::ArcSwapOption;

use super::config::WebFetchParams;
use super::error::WebFetchError;

/// Cached, invalidatable HTTP client for web fetching.
///
/// - **Normal path:** `get_or_rebuild()` returns the cached client via a
///   lock-free atomic load.
/// - **On transport error:** call `invalidate()` to atomically set the
///   client to `None`. The next `get_or_rebuild()` falls through and
///   builds a fresh client with a clean connection pool.
#[derive(Clone, Debug)]
pub(crate) struct HttpClient {
    inner: Arc<ArcSwapOption<reqwest::Client>>,
    params: WebFetchParams,
}

impl HttpClient {
    pub(crate) fn new(params: &WebFetchParams) -> Result<Self, WebFetchError> {
        // Validate the proxy eagerly; the transport builds lazily
        // (`invalidate()` promises a fresh pool).
        if let Some(ref endpoint) = params.proxy_endpoint {
            reqwest::Proxy::all(endpoint)
                .map_err(|e| WebFetchError::ProxyConfigError(e.to_string()))?;
        }
        Ok(Self {
            inner: Arc::new(ArcSwapOption::from(None)),
            params: params.clone(),
        })
    }

    /// Get the current client, rebuilding if it was invalidated.
    pub(crate) fn get_or_rebuild(&self) -> Result<Arc<reqwest::Client>, WebFetchError> {
        // Fast path: lock-free atomic load.
        if let Some(client) = self.inner.load_full() {
            return Ok(client);
        }
        // Client was invalidated — rebuild with a fresh connection pool.
        let fresh = Arc::new(Self::build(&self.params)?);
        self.inner.store(Some(Arc::clone(&fresh)));
        Ok(fresh)
    }

    /// Atomically invalidate the cached client. The next `get_or_rebuild()`
    /// will construct a fresh one with a clean connection pool.
    pub(crate) fn invalidate(&self) {
        self.inner.store(None);
    }

    fn build(params: &WebFetchParams) -> Result<reqwest::Client, WebFetchError> {
        // Route all traffic through the egress proxy when configured.
        let proxy = params
            .proxy_endpoint
            .as_ref()
            .map(reqwest::Proxy::all)
            .transpose()
            .map_err(|e| WebFetchError::ProxyConfigError(e.to_string()))?;
        echo_agent_extra_ca::build_reqwest_client(|builder| {
            let mut builder = builder
                .timeout(params.timeout_secs())
                .connect_timeout(std::time::Duration::from_secs(10))
                // We manage redirects for SSRF.
                .redirect(reqwest::redirect::Policy::none())
                .pool_max_idle_per_host(2)
                .pool_idle_timeout(std::time::Duration::from_secs(30))
                .tcp_nodelay(true)
                // Reduce size of incoming payloads.
                .gzip(true)
                .brotli(true)
                .deflate(true);
            if let Some(proxy) = proxy.clone() {
                builder = builder.proxy(proxy);
            }
            builder
        })
        .map_err(WebFetchError::ClientBuildError)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_or_rebuild_returns_client() {
        let client = HttpClient::new(&WebFetchParams::default()).unwrap();
        let http = client.get_or_rebuild().unwrap();
        assert!(Arc::strong_count(&http) >= 1);
    }

    #[test]
    fn invalidate_forces_rebuild() {
        let client = HttpClient::new(&WebFetchParams::default()).unwrap();
        let first = client.get_or_rebuild().unwrap();
        let first_ptr = Arc::as_ptr(&first);

        client.invalidate();

        let second = client.get_or_rebuild().unwrap();
        let second_ptr = Arc::as_ptr(&second);

        // After invalidation, we should get a different client instance.
        assert_ne!(first_ptr, second_ptr);
    }

    #[test]
    fn build_with_proxy_endpoint() {
        let params = WebFetchParams {
            proxy_endpoint: Some("https://proxy.corp.example.com".into()),
            ..Default::default()
        };
        // Should succeed — reqwest accepts the proxy URL.
        let client = HttpClient::new(&params);
        assert!(client.is_ok());
    }

    #[test]
    fn build_without_proxy_is_default() {
        let params = WebFetchParams::default();
        assert!(params.proxy_endpoint.is_none());
        let client = HttpClient::new(&params);
        assert!(client.is_ok());
    }

    #[test]
    fn build_with_invalid_proxy_endpoint() {
        let params = WebFetchParams {
            proxy_endpoint: Some("not a valid url".into()),
            ..Default::default()
        };
        let result = HttpClient::new(&params);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("proxy"),
            "Expected proxy-related error, got: {err}"
        );
    }
}
