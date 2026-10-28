//! Embedding provider abstraction for memory vector search.
//!
//! Defines the `EmbeddingProvider` trait and an API-based implementation
//! that calls an OpenAI-compatible embeddings API endpoint.
//!
//! Embeddings are cached in the sqlite-vec `chunks_vec` table — the vec0
//! virtual table IS the cache. No separate cache needed.

use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryPolicyError {
    Disabled,
    ContentVetoed,
    LedgerUnavailable(String),
}

impl std::fmt::Display for MemoryPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => f.write_str("memory is disabled"),
            Self::ContentVetoed => f.write_str("memory content excluded by privacy policy"),
            Self::LedgerUnavailable(error) => write!(f, "memory ledger unavailable: {error}"),
        }
    }
}
impl std::error::Error for MemoryPolicyError {}

pub type MemoryRequestPermit =
    std::sync::Arc<dyn Fn(&[&str]) -> Result<(), MemoryPolicyError> + Send + Sync>;

/// Maximum retry attempts for transient API errors (429, 5xx).
const MAX_RETRIES: usize = 3;
/// Initial backoff delay in milliseconds (doubles on each retry: 1s, 2s, 4s).
const INITIAL_BACKOFF_MS: u64 = 1000;

/// Trait for generating text embeddings.
///
/// Implementations must be `Send + Sync` so they can be used in `Send`
/// futures (e.g., inside `tokio::spawn`). The `embed_batch` method is
/// async to support API-based providers.
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a batch of texts, returning one vector per input text.
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>>;

    /// Bind candidate provenance to every send/retry, including auth replay.
    async fn embed_batch_with_policy(
        &self,
        texts: &[&str],
        permit: MemoryRequestPermit,
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        permit(texts)?;
        self.embed_batch(texts).await
    }

    /// The model name used for embeddings.
    fn model_name(&self) -> &str;

    /// The dimensionality of the embedding vectors.
    fn dimensions(&self) -> usize;

    /// Stable identity of the embedding space used for cache invalidation.
    /// Providers may override this when the endpoint is part of model identity.
    fn fingerprint(&self) -> String {
        format!("{}:{}", self.model_name(), self.dimensions())
    }
}

pub(crate) fn validate_embedding_batch(
    embeddings: &[Vec<f32>],
    expected_count: usize,
    expected_dimensions: usize,
) -> Result<(), String> {
    if embeddings.len() != expected_count {
        return Err(format!(
            "embedding provider returned {} vectors for {expected_count} inputs",
            embeddings.len()
        ));
    }
    for (index, embedding) in embeddings.iter().enumerate() {
        if embedding.len() != expected_dimensions {
            return Err(format!(
                "embedding {index} has {} dimensions; expected {expected_dimensions}",
                embedding.len()
            ));
        }
        if embedding.iter().any(|value| !value.is_finite()) {
            return Err(format!("embedding {index} contains a non-finite value"));
        }
    }
    Ok(())
}

/// API-based embedding provider using an OpenAI-compatible embeddings endpoint.
#[derive(Clone)]
pub struct ApiEmbeddingProvider {
    api_base: String,
    model: String,
    dimensions: usize,
    client: reqwest_middleware::ClientWithMiddleware,
    max_batch_size: usize,
    send_dimensions: bool,
    request_permit: Option<MemoryRequestPermit>,
}

impl ApiEmbeddingProvider {
    pub fn new(
        api_base: String,
        model: String,
        dimensions: usize,
        client: reqwest_middleware::ClientWithMiddleware,
    ) -> Self {
        Self {
            api_base,
            model,
            dimensions,
            client,
            max_batch_size: 32,
            send_dimensions: true,
            request_permit: None,
        }
    }

    /// Recheck live memory policy immediately before every batch and retry.
    pub fn with_request_permit(mut self, permit: MemoryRequestPermit) -> Self {
        self.request_permit = Some(permit);
        self
    }

    pub fn from_config(
        config: &echo_agent_config_types::MemoryEmbeddingConfig,
        api_base: String,
        client: reqwest_middleware::ClientWithMiddleware,
    ) -> Option<Self> {
        let model = config.model.clone().filter(|m| !m.is_empty())?;
        let mut provider = Self::new(api_base, model, config.dimensions, client);
        provider.send_dimensions = config.send_dimensions;
        Some(provider)
    }

    pub fn from_session(
        config: &echo_agent_config_types::MemoryEmbeddingConfig,
        proxy_base_url: String,
        auth_key: String,
    ) -> Option<Self> {
        let client =
            build_static_middleware_client((!auth_key.trim().is_empty()).then_some(auth_key));
        Self::from_config(config, proxy_base_url, client)
    }

    fn endpoint_url(&self) -> String {
        let base = self.api_base.trim_end_matches('/');
        if base.ends_with("/embeddings") {
            base.to_owned()
        } else {
            format!("{base}/embeddings")
        }
    }

    fn request_body(&self, input: Vec<&str>) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": self.model,
            "input": input,
        });
        if self.send_dimensions {
            body["dimensions"] = serde_json::json!(self.dimensions);
        }
        body
    }
}

pub(super) fn build_middleware_client(
    credentials: std::sync::Arc<dyn echo_agent_auth::AuthCredentialProvider>,
) -> reqwest_middleware::ClientWithMiddleware {
    echo_agent_http::with_auth_retry(echo_agent_http::shared_client(), credentials)
}

fn build_static_middleware_client(
    api_key: Option<String>,
) -> reqwest_middleware::ClientWithMiddleware {
    let provider: std::sync::Arc<dyn echo_agent_auth::AuthCredentialProvider> = std::sync::Arc::new(
        echo_agent_auth::StaticAuthCredentialProvider::new(Box::new(NoopHttpAuth), api_key),
    );
    build_middleware_client(provider)
}

struct NoopHttpAuth;

impl echo_agent_auth::HttpAuth for NoopHttpAuth {
    fn apply(&self, builder: reqwest::RequestBuilder, _base_url: &str) -> reqwest::RequestBuilder {
        builder
    }
}

#[async_trait]
impl EmbeddingProvider for ApiEmbeddingProvider {
    async fn embed_batch_with_policy(
        &self,
        texts: &[&str],
        permit: MemoryRequestPermit,
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        let previous = self.request_permit.clone();
        let provider = self
            .clone()
            .with_request_permit(std::sync::Arc::new(move |texts| {
                if let Some(previous) = &previous {
                    previous(texts)?;
                }
                permit(texts)
            }));
        provider.embed_batch(texts).await
    }
    #[tracing::instrument(name = "memory.embed_batch", skip_all, fields(batch_size = texts.len()))]
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());

        // Process in batches to respect API payload limits
        for batch in texts.chunks(self.max_batch_size) {
            let input: Vec<&str> = batch.to_vec();
            let body_json = self.request_body(input);

            // Retry with exponential backoff on transient errors (429, 5xx)
            let mut last_err = String::new();
            let mut success = false;
            for attempt in 0..MAX_RETRIES {
                if attempt > 0 {
                    let delay = INITIAL_BACKOFF_MS * 2u64.pow(attempt as u32 - 1);
                    tracing::warn!(
                        attempt,
                        delay_ms = delay,
                        "retrying embedding API call after transient error"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }

                if let Some(permit) = &self.request_permit {
                    permit(batch)?;
                }
                let request = echo_agent_http::shared_client()
                    .post(self.endpoint_url())
                    .json(&body_json)
                    .header("X-Echo-Agent-Token-Auth", "echo-agent-cli")
                    .header("x-echo-agent-client-version", echo_agent_version::VERSION);

                let req = match request.build() {
                    Ok(r) => r,
                    Err(e) => {
                        return Err(format!("failed to build embedding request: {e}").into());
                    }
                };
                let response = match if let Some(permit) = &self.request_permit {
                    let permit = permit.clone();
                    let texts: Vec<String> = batch.iter().map(|text| (*text).to_owned()).collect();
                    echo_agent_auth::retry_middleware::execute_with_permit(
                        &self.client,
                        req,
                        echo_agent_auth::retry_middleware::RequestPermit(std::sync::Arc::new(
                            move || {
                                let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
                                permit(&refs).is_ok()
                            },
                        )),
                    )
                    .await
                } else {
                    self.client.execute(req).await
                } {
                    Ok(r) => r,
                    Err(e) => {
                        last_err = format!("request failed: {e}");
                        continue;
                    }
                };

                let status = response.status();
                if status.is_success() {
                    let body: serde_json::Value = response.json().await?;
                    let data = body
                        .get("data")
                        .and_then(|d| d.as_array())
                        .ok_or("embedding response missing 'data' array")?;

                    let mut ordered = vec![None; batch.len()];
                    for item in data {
                        let index = item
                            .get("index")
                            .and_then(|value| value.as_u64())
                            .and_then(|value| usize::try_from(value).ok())
                            .filter(|index| *index < batch.len())
                            .ok_or("embedding item has an invalid index")?;
                        if ordered[index].is_some() {
                            return Err(format!("embedding response repeats index {index}").into());
                        }
                        let embedding: Vec<f32> = item
                            .get("embedding")
                            .and_then(|e| e.as_array())
                            .ok_or("embedding item missing 'embedding' array")?
                            .iter()
                            .map(|value| {
                                value
                                    .as_f64()
                                    .filter(|number| number.is_finite())
                                    .map(|number| number as f32)
                                    .filter(|number| number.is_finite())
                                    .ok_or("embedding contains a non-finite or non-numeric value")
                            })
                            .collect::<Result<_, _>>()?;
                        ordered[index] = Some(embedding);
                    }
                    let batch_embeddings = ordered
                        .into_iter()
                        .collect::<Option<Vec<_>>>()
                        .ok_or("embedding response omitted one or more inputs")?;
                    validate_embedding_batch(&batch_embeddings, batch.len(), self.dimensions)
                        .map_err(|error| format!("invalid embedding response: {error}"))?;
                    all_embeddings.extend(batch_embeddings);
                    success = true;
                    break;
                }

                // Retry on 429 (rate limit) or 5xx (server error)
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    last_err = format!(
                        "HTTP {status}: {}",
                        response.text().await.unwrap_or_default()
                    );
                    continue;
                }

                // Non-retryable error (4xx other than 429)
                let body = response.text().await.unwrap_or_default();
                return Err(format!("embedding API error {status}: {body}").into());
            }

            if !success {
                return Err(format!(
                    "embedding API failed after {MAX_RETRIES} attempts: {last_err}"
                )
                .into());
            }
        }

        Ok(all_embeddings)
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn fingerprint(&self) -> String {
        // Do not persist endpoint URLs (which may contain tenant identifiers),
        // but do distinguish equal model names served by different backends.
        let endpoint_hash = blake3::hash(self.api_base.as_bytes());
        format!(
            "api:{}:{}:{}",
            &endpoint_hash.to_hex()[..16],
            self.model,
            self.dimensions
        )
    }
}

/// A mock embedding provider for testing that returns deterministic vectors.
/// Uses blake3 hash of text → float values for reproducible results.
#[cfg(any(test, feature = "test-support"))]
pub struct MockEmbeddingProvider {
    pub dimensions: usize,
}

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<Vec<f32>>, Box<dyn std::error::Error>> {
        Ok(texts
            .iter()
            .map(|text| {
                let hash = blake3::hash(text.as_bytes());
                let bytes = hash.as_bytes();
                (0..self.dimensions)
                    .map(|i| bytes[i % 32] as f32 / 255.0)
                    .collect()
            })
            .collect())
    }

    fn model_name(&self) -> &str {
        "mock-embedding"
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn live_policy_blocks_later_embedding_batches_and_embedding_rerank_retries() {
        use std::io::{Read, Write};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };
        for mode in ["embedding-retry", "embedding-batch", "rerank-retry"] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let allowed = Arc::new(AtomicBool::new(true));
            let stopped = Arc::new(AtomicBool::new(false));
            let received = Arc::new(AtomicUsize::new(0));
            let (live, stop, count) = (allowed.clone(), stopped.clone(), received.clone());
            let server = std::thread::spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 2048];
                    loop {
                        let size = stream.read(&mut buffer).unwrap();
                        assert!(size > 0);
                        bytes.extend_from_slice(&buffer[..size]);
                        if let Some(start) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..start]);
                            let length: usize = headers
                                .lines()
                                .find_map(|line| {
                                    let (key, value) = line.split_once(':')?;
                                    key.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse().unwrap())
                                })
                                .unwrap();
                            if bytes.len() >= start + 4 + length {
                                break;
                            }
                        }
                    }
                    count.fetch_add(1, Ordering::AcqRel);
                    live.store(false, Ordering::Release);
                    let (status, body) = if mode == "embedding-batch" {
                        ("200 OK", r#"{"data":[{"index":0,"embedding":[1.0,0.0]}]}"#)
                    } else {
                        ("500 Internal Server Error", "{}")
                    };
                    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            });
            let permit: MemoryRequestPermit = Arc::new(move |_| {
                if allowed.load(Ordering::Acquire) {
                    Ok(())
                } else {
                    Err(MemoryPolicyError::Disabled)
                }
            });
            let error = if mode == "rerank-retry" {
                let config = echo_agent_config_types::MemoryRerankerConfig {
                    enabled: true,
                    endpoint: Some(endpoint),
                    model: Some("test".into()),
                    ..Default::default()
                };
                let provider = crate::reranker::ApiReranker::from_config(&config)
                    .unwrap()
                    .with_request_permit(permit);
                provider
                    .rerank(
                        "query",
                        &[crate::search::SearchResult {
                            chunk_id: "fact".into(),
                            path: "memory.md".into(),
                            start_line: 1,
                            end_line: 1,
                            score: 1.0,
                            snippet: "fact".into(),
                            source: "global".into(),
                            created_at: 0,
                        }],
                        1,
                    )
                    .await
                    .unwrap_err()
                    .to_string()
            } else {
                let mut provider = ApiEmbeddingProvider::new(
                    endpoint,
                    "test".into(),
                    2,
                    build_static_middleware_client(None),
                )
                .with_request_permit(permit);
                provider.max_batch_size = 1;
                provider
                    .embed_batch(&["first", "second"])
                    .await
                    .unwrap_err()
                    .to_string()
            };
            stopped.store(true, Ordering::Release);
            server.join().unwrap();
            assert!(error.contains("memory is disabled"), "{mode}: {error}");
            assert_eq!(received.load(Ordering::Acquire), 1, "{mode}");
        }
    }

    #[tokio::test]
    async fn test_mock_embedding_deterministic() {
        let provider = MockEmbeddingProvider { dimensions: 4 };
        let r1 = provider.embed_batch(&["hello"]).await.unwrap();
        let r2 = provider.embed_batch(&["hello"]).await.unwrap();
        assert_eq!(r1, r2);
    }

    #[tokio::test]
    async fn test_mock_embedding_different_texts() {
        let provider = MockEmbeddingProvider { dimensions: 4 };
        let results = provider.embed_batch(&["hello", "world"]).await.unwrap();
        assert_eq!(results.len(), 2);
        assert_ne!(results[0], results[1]);
    }

    #[tokio::test]
    async fn test_mock_embedding_empty_input() {
        let provider = MockEmbeddingProvider { dimensions: 4 };
        let results = provider.embed_batch(&[]).await.unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn embedding_endpoint_accepts_base_or_full_url() {
        let client = build_static_middleware_client(Some("test-key".to_owned()));
        let from_base = ApiEmbeddingProvider::new(
            "https://example.com/v1/".to_owned(),
            "test-model".to_owned(),
            4,
            client.clone(),
        );
        let full = ApiEmbeddingProvider::new(
            "https://example.com/v1/embeddings".to_owned(),
            "test-model".to_owned(),
            4,
            client,
        );

        assert_eq!(
            from_base.endpoint_url(),
            "https://example.com/v1/embeddings"
        );
        assert_eq!(full.endpoint_url(), "https://example.com/v1/embeddings");
    }

    #[test]
    fn fixed_dimension_provider_omits_dimensions_parameter() {
        let config = echo_agent_config_types::MemoryEmbeddingConfig {
            model: Some("fixed-dimension-model".to_owned()),
            send_dimensions: false,
            ..Default::default()
        };
        let provider = ApiEmbeddingProvider::from_config(
            &config,
            "https://example.com/v1/embeddings".to_owned(),
            build_static_middleware_client(Some("test-key".to_owned())),
        )
        .unwrap();

        let body = provider.request_body(vec!["test"]);
        assert!(body.get("dimensions").is_none());
    }

    #[tokio::test]
    async fn test_mock_embedding_correct_dimensions() {
        let provider = MockEmbeddingProvider { dimensions: 128 };
        let results = provider.embed_batch(&["test"]).await.unwrap();
        assert_eq!(results[0].len(), 128);
    }

    #[test]
    fn validation_rejects_partial_wrong_dimension_and_non_finite_batches() {
        assert!(validate_embedding_batch(&[vec![0.0, 1.0]], 2, 2).is_err());
        assert!(validate_embedding_batch(&[vec![0.0]], 1, 2).is_err());
        assert!(validate_embedding_batch(&[vec![0.0, f32::NAN]], 1, 2).is_err());
        assert!(validate_embedding_batch(&[vec![0.0, 1.0]], 1, 2).is_ok());
    }
}
