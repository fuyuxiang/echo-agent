//! API-backed relevance re-ranking for memory search candidates.

use std::collections::HashSet;

use super::search::SearchResult;
use echo_agent_config_types::MemoryRerankerConfig;

const MAX_RETRIES: usize = 3;
const INITIAL_BACKOFF_MS: u64 = 500;

pub struct ApiReranker {
    endpoint: String,
    model: String,
    api_key: Option<String>,
    request_permit: Option<super::embedding::MemoryRequestPermit>,
}

impl ApiReranker {
    pub fn from_config(config: &MemoryRerankerConfig) -> Option<Self> {
        if !config.is_configured() {
            return None;
        }
        Some(Self {
            endpoint: config.endpoint.clone()?,
            model: config.model.clone()?,
            api_key: config.api_key.clone().filter(|key| !key.trim().is_empty()),
            request_permit: None,
        })
    }

    pub fn with_request_permit(mut self, permit: super::embedding::MemoryRequestPermit) -> Self {
        self.request_permit = Some(permit);
        self
    }

    fn request_body(&self, query: &str, documents: &[&str], count: usize) -> serde_json::Value {
        serde_json::json!({
            "model": self.model,
            "query": query,
            "documents": documents,
            "return_documents": false,
            "top_n": count,
        })
    }

    #[tracing::instrument(name = "memory.rerank", skip_all, fields(candidate_count = candidates.len(), top_n))]
    pub async fn rerank(
        &self,
        query: &str,
        candidates: &[SearchResult],
        top_n: usize,
    ) -> Result<Vec<SearchResult>, Box<dyn std::error::Error + Send + Sync>> {
        if candidates.is_empty() || top_n == 0 {
            return Ok(Vec::new());
        }

        let documents: Vec<&str> = candidates
            .iter()
            .map(|candidate| candidate.snippet.as_str())
            .collect();
        let body = self.request_body(query, &documents, top_n.min(candidates.len()));

        let mut last_error = String::new();
        for attempt in 0..MAX_RETRIES {
            if attempt > 0 {
                let delay = INITIAL_BACKOFF_MS * 2u64.pow(attempt as u32 - 1);
                tracing::warn!(attempt, delay_ms = delay, "retrying reranker API call");
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }

            if let Some(permit) = &self.request_permit {
                permit(&documents)?;
            }
            let mut request =
                echo_agent_http::shared_client().post(self.endpoint.trim_end_matches('/'));
            if let Some(key) = self.api_key.as_deref() {
                request = request.bearer_auth(key);
            }
            let response = match request.json(&body).send().await {
                Ok(response) => response,
                Err(error) => {
                    last_error = format!("request failed: {error}");
                    continue;
                }
            };

            let status = response.status();
            if status.is_success() {
                let response_body: serde_json::Value = response.json().await?;
                return reorder_from_response(candidates, &response_body, top_n)
                    .map_err(Into::into);
            }

            let response_body = response.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                last_error = format!("HTTP {status}: {response_body}");
                continue;
            }
            return Err(format!("reranker API error {status}: {response_body}").into());
        }

        Err(format!("reranker API failed after {MAX_RETRIES} attempts: {last_error}").into())
    }
}

fn reorder_from_response(
    candidates: &[SearchResult],
    body: &serde_json::Value,
    top_n: usize,
) -> Result<Vec<SearchResult>, String> {
    let items = body
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "reranker response missing 'results' array".to_owned())?;
    let mut seen = HashSet::with_capacity(items.len());
    let mut reranked = Vec::with_capacity(items.len().min(top_n));

    for item in items.iter().take(top_n) {
        let index = item
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .filter(|index| *index < candidates.len())
            .ok_or_else(|| "reranker result contains an invalid index".to_owned())?;
        if !seen.insert(index) {
            return Err(format!("reranker response repeats index {index}"));
        }
        let score = item
            .get("relevance_score")
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("reranker result {index} has an invalid relevance_score"))?;

        let mut result = candidates[index].clone();
        result.score = score.clamp(0.0, 1.0);
        reranked.push(result);
    }

    if reranked.is_empty() {
        return Err("reranker response contains no results".to_owned());
    }
    Ok(reranked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyless_reranker_configuration_is_usable() {
        let config = MemoryRerankerConfig {
            enabled: true,
            endpoint: Some("http://123.56.188.16:8088/v1/rerank".into()),
            model: Some("rerank-pro".into()),
            api_key: None,
        };
        let client = ApiReranker::from_config(&config).unwrap();
        assert!(client.api_key.is_none());
    }

    #[test]
    fn reranker_requests_only_the_needed_results() {
        let config = MemoryRerankerConfig {
            enabled: true,
            endpoint: Some("https://example.com/v1/rerank".into()),
            model: Some("generic".into()),
            ..Default::default()
        };
        let body =
            ApiReranker::from_config(&config)
                .unwrap()
                .request_body("query", &["candidate"], 1);
        assert_eq!(
            body.get("top_n").and_then(serde_json::Value::as_u64),
            Some(1)
        );
        assert!(body.get("top_k").is_none());
    }

    fn candidate(id: &str) -> SearchResult {
        SearchResult {
            chunk_id: id.to_owned(),
            path: format!("{id}.md"),
            start_line: 1,
            end_line: 2,
            score: 0.25,
            snippet: format!("content {id}"),
            source: "workspace".to_owned(),
            created_at: 1,
        }
    }

    #[test]
    fn response_order_and_scores_replace_coarse_ranking() {
        let candidates = vec![candidate("a"), candidate("b"), candidate("c")];
        let body = serde_json::json!({
            "results": [
                { "index": 2, "relevance_score": 0.91 },
                { "index": 0, "relevance_score": 0.72 }
            ]
        });

        let reranked = reorder_from_response(&candidates, &body, 2).unwrap();
        assert_eq!(reranked[0].chunk_id, "c");
        assert_eq!(reranked[1].chunk_id, "a");
        assert!((reranked[0].score - 0.91).abs() < f64::EPSILON);
    }

    #[test]
    fn duplicate_indices_are_rejected() {
        let candidates = vec![candidate("a")];
        let body = serde_json::json!({
            "results": [
                { "index": 0, "relevance_score": 0.9 },
                { "index": 0, "relevance_score": 0.8 }
            ]
        });

        assert!(reorder_from_response(&candidates, &body, 2).is_err());
    }
}
